use std::sync::Arc;
use std::time::Instant;

use actix_http::encoding::Decoder;
use actix_http::BoxedPayloadStream;
use actix_http::Payload;
use actix_http::Uri;
use awc::http::Method;
use awc::http::StatusCode;
use ccx_api_lib::make_client;
use ccx_api_lib::Client;
use ccx_api_lib::ClientRequest;
use ccx_api_lib::ClientResponse;
use serde::Deserialize;
use serde::Serialize;

use super::*;
// use crate::client::limits::UsedRateLimits;
use crate::client::WebsocketStream;
use crate::error::*;
// use crate::proto::TimeWindow;

/// API client.
pub struct RestClient<S>
where
    S: KrakenSigner,
{
    inner: Arc<ClientInner<S>>,
}

impl<S> Clone for RestClient<S>
where
    S: KrakenSigner,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct ClientInner<S>
where
    S: KrakenSigner,
{
    config: Config<S>,
}

pub struct RequestBuilder<S>
where
    S: KrakenSigner,
{
    api_client: RestClient<S>,
    request: ClientRequest,
    sign: Option<(Nonce,)>,
    body: String,
}

#[derive(Serialize, Deserialize)]
struct KrakenApiAnswer<T> {
    result: Option<T>,
    error: Vec<String>,
}

impl<T> KrakenApiAnswer<T>
where
    T: Deserialize<'static>,
{
    fn into_api_result(self) -> KrakenApiResult<T> {
        let error = KrakenApiError(self.error.into_iter().map(ApiError::from_string).collect());
        match self.result {
            Some(result) => Ok((result, error)),
            None => Err(error)?,
        }
    }
}

impl<S> RestClient<S>
where
    S: KrakenSigner,
{
    pub fn new(config: Config<S>) -> Self {
        let inner = Arc::new(ClientInner { config });
        RestClient { inner }
    }

    pub(super) fn client(&self) -> Client {
        make_client(false, self.inner.config.proxy.as_ref())
    }

    pub fn request(&self, method: Method, endpoint: &str) -> KrakenResult<RequestBuilder<S>> {
        let url = self.inner.config.api_base.join(endpoint)?;
        log::debug!("Requesting: {}", url.as_str());
        let api_client = self.clone();
        let request = self.client().request(method, url.as_str());
        Ok(RequestBuilder {
            api_client,
            request,
            sign: None,
            body: String::new(),
        })
    }

    pub fn get(&self, endpoint: &str) -> KrakenResult<RequestBuilder<S>> {
        self.request(Method::GET, endpoint)
    }

    pub fn post(&self, endpoint: &str) -> KrakenResult<RequestBuilder<S>> {
        self.request(Method::POST, endpoint)
    }

    pub fn put(&self, endpoint: &str) -> KrakenResult<RequestBuilder<S>> {
        self.request(Method::PUT, endpoint)
    }

    pub fn delete(&self, endpoint: &str) -> KrakenResult<RequestBuilder<S>> {
        self.request(Method::DELETE, endpoint)
    }

    pub async fn web_socket(&self) -> KrakenResult<WebsocketStream> {
        let url = self.inner.config.stream_base.clone();
        WebsocketStream::connect(self.clone(), url).await
    }
}

impl<S> RequestBuilder<S>
where
    S: KrakenSigner,
{
    pub fn uri(&self) -> String {
        self.request.get_uri().to_string()
    }

    pub fn query_arg<Name: AsRef<str>, T: Serialize + ?Sized>(
        mut self,
        name: Name,
        query: &T,
    ) -> KrakenResult<Self> {
        let mut parts = self.request.get_uri().clone().into_parts();

        if let Some(path_and_query) = parts.path_and_query {
            let mut buf = path_and_query.path().to_string();
            buf.push('?');
            match path_and_query.query().unwrap_or("") {
                "" => {}
                old_query => {
                    buf.push_str(old_query);
                    buf.push('&');
                }
            }
            buf.push_str(&serde_urlencoded::to_string([(name.as_ref(), query)])?);
            parts.path_and_query = buf.parse().ok();
            let uri = Uri::from_parts(parts).map_err(|e| KrakenError::other(format!("{:?}", e)))?;
            self.request = self.request.uri(uri);
        }

        Ok(self)
    }

    pub fn try_query_arg<Name: AsRef<str>, T: Serialize>(
        self,
        name: Name,
        query: &Option<T>,
    ) -> KrakenResult<Self> {
        match query {
            Some(val) => self.query_arg(name, val),
            None => Ok(self),
        }
    }

    pub fn auth_header(mut self) -> KrakenResult<Self> {
        self.request = self
            .request
            .append_header(("API-Key", self.api_client.inner.config.api_key()));
        Ok(self)
    }

    pub fn request_body(mut self, payload: impl Serialize) -> KrakenResult<Self> {
        self.body = match &self.sign {
            Some((nonce,)) => serde_urlencoded::to_string(nonce.wrap(payload)),
            None => serde_urlencoded::to_string(payload),
        }?;
        Ok(self)
    }

    pub fn signed(mut self, nonce: Nonce) -> KrakenResult<Self> {
        self.sign = Some((nonce,));
        self.auth_header()
    }

    pub async fn send<V>(mut self) -> KrakenApiResult<V>
    where
        V: serde::de::DeserializeOwned,
    {
        self = self.sign().await?;
        self.request = self
            .request
            .content_type("application/x-www-form-urlencoded");
        log::debug!("{}  {}", self.request.get_method(), self.request.get_uri());
        log::debug!("{}", self.body);
        let tm = Instant::now();
        let mut res = self.request.send_body(self.body).await?;
        let d1 = tm.elapsed();
        let resp = res.body().limit(16 * 1024 * 1024).await?;
        let d2 = tm.elapsed() - d1;
        log::debug!(
            "Request time elapsed:  {:0.1}ms + {:0.1}ms",
            d1.as_secs_f64() * 1000.0,
            d2.as_secs_f64() * 1000.0,
        );
        log::debug!(
            "Response: {} «{}»",
            res.status(),
            String::from_utf8_lossy(&resp)
        );
        if let Err(err) = check_response(res) {
            // log::debug!("Response: {}", String::from_utf8_lossy(&resp));
            Err(err)?
        };
        let answer: KrakenApiAnswer<V> = match serde_json::from_slice(&resp) {
            Ok(json) => json,
            Err(err) => {
                // log::debug!("Response: {}", String::from_utf8_lossy(&resp));
                Err(err)?
            }
        };
        answer.into_api_result()
    }

    // pub async fn send_no_response(mut self) -> KrakenResult<()> {
    //     self = self.sign()?;
    //     log::debug!("{}  {}", self.request.get_method(), self.request.get_uri(),);
    //     let tm = Instant::now();
    //     let mut res = self.request.send().await?;
    //     let d1 = tm.elapsed();
    //     let resp = res.body().limit(16 * 1024 * 1024).await?;
    //     let d2 = tm.elapsed() - d1;
    //     log::debug!(
    //         "Request time elapsed:  {:0.1}ms + {:0.1}ms",
    //         d1.as_secs_f64() * 1000.0,
    //         d2.as_secs_f64() * 1000.0,
    //     );
    //     log::debug!(
    //         "Response: {} «{}»",
    //         res.status(),
    //         String::from_utf8_lossy(&resp)
    //     );
    //     if let Err(err) = check_response(res) {
    //         // log::debug!("Response: {}", String::from_utf8_lossy(&resp));
    //         Err(err)?
    //     };
    //     Ok(())
    // }

    async fn sign(mut self) -> KrakenResult<Self> {
        if let Some((nonce,)) = self.sign {
            let path = self.request.get_uri().path();
            let signature = self
                .api_client
                .inner
                .config
                .signer()
                .sign_data(nonce, path, &self.body)
                .await?;
            self.request = self.request.append_header(("API-Sign", signature));
        };
        Ok(self)
    }
}

/// Return base64 encoded api sign.

type AwcClientResponse = ClientResponse<Decoder<Payload<BoxedPayloadStream>>>;

fn check_response(res: AwcClientResponse) -> KrakenApiResult<AwcClientResponse> {
    // let used_rate_limits = UsedRateLimits::from_headers(res.headers());
    //
    // log::debug!("  used_rate_limits:  {:?}", used_rate_limits);

    match res.status() {
        StatusCode::OK => KrakenApiError::ok(res),
        StatusCode::INTERNAL_SERVER_ERROR => Err(ApiServiceError::ServerError)?,
        StatusCode::SERVICE_UNAVAILABLE => Err(ApiServiceError::ServiceUnavailable)?,
        // StatusCode::UNAUTHORIZED => Err(RequestError::Unauthorized)?,
        // StatusCode::BAD_REQUEST => {
        //     let error_json: BinanceContentError = response.json()?;
        //
        //     Err(ErrorKind::BinanceError(error_json.code, error_json.msg, response).into())
        // }
        s => Err(KrakenError::UnknownStatus(s))?,
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//
//     #[test]
//     fn it_should_sign() {
//         #[derive(Serialize)]
//         struct Payload {
//             ordertype: &'static str,
//             pair: &'static str,
//             price: &'static str,
//             r#type: &'static str,
//             volume: &'static str,
//         }
//         let encoded_secret = "kQH5HW/8p1uGOVjbgWA7FunAmGO8lsSUXNsu3eow76sz84Q18fWxnyRzBHCd3pd5nE9qa99HAZtuZuj6F1huXg==";
//         let decoded_secret = base64::decode(&encoded_secret).unwrap();
//         let nonce = Nonce::new(1616492376594_u64);
//         // nonce=1616492376594&ordertype=limit&pair=XBTUSD&price=37500&type=buy&volume=1.25
//         let payload = Payload {
//             ordertype: "limit",
//             pair: "XBTUSD",
//             price: "37500",
//             r#type: "buy",
//             volume: "1.25",
//         };
//         let path = "/0/private/AddOrder";
//         let wrapped = nonce.wrap(&payload);
//         let body = serde_urlencoded::to_string(wrapped).unwrap();
//         let res = sign(path, nonce, &body, &decoded_secret);
//         assert_eq!(
//             res,
//             "4/dpxb3iT4tp/ZCVEwSnEsLxx0bqyhLpdfOpc6fn7OR8+UClSV5n9E6aSS8MPtnRfp32bAb0nmbRn6H8ndwLUQ=="
//         )
//     }
// }
