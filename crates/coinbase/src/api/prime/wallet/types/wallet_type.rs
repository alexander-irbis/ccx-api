#[cfg(feature = "db")]
use diesel_derives::AsExpression;
#[cfg(feature = "db")]
use diesel_derives::FromSqlRow;

use crate::api::prime::prelude::*;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Eq, PartialEq)]
#[cfg_attr(feature = "db", derive(AsExpression, FromSqlRow))]
#[cfg_attr(feature = "db", sql_type = "diesel::sql_types::Text")]
pub enum PortfolioWalletType {
    /// A crypto vault.
    #[serde(rename = "VAULT")]
    Vault,
    /// A trading wallet.
    #[serde(rename = "TRADING")]
    Trading,
    /// Other wallet types (like consumer, etc.).
    #[serde(rename = "WALLET_TYPE_OTHER")]
    Other,
    /// A QC wallet.
    #[serde(rename = "QC")]
    Qc,
    /// An Onchain wallet
    #[serde(rename = "ONCHAIN")]
    Onchain,
}
#[cfg(feature = "db")]
forward_display_to_serde!(AccountPortfolioWalletType);
#[cfg(feature = "db")]
forward_from_str_to_serde!(AccountPortfolioWalletType);

// impl PortfolioWalletType {
//     pub fn from_name(name: &str) -> Option<Self> {
//         Self::from_str(name).ok()
//     }
//
//     pub fn name(&self) -> String {
//         self.to_string()
//     }
// }
