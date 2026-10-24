//! Provider 账号领域、持久化端口与同一 target 内的账号选择

mod error;
mod fast_mode;
pub use fast_mode::FastMode;
mod location;
pub use location::{InvalidRequestLocation, RequestLocation};
mod model;
mod model_access;
mod model_mapping;
pub use model_access::{
    AccountModelAccess, AccountModelAccessMode, InvalidAccountModelAccess,
    MAX_ACCOUNT_ACCESS_MODELS,
};
pub use model_mapping::{
    AccountModelMappings, InvalidAccountModelMappings, MAX_ACCOUNT_MODEL_MAPPINGS,
};
mod proxy;
pub use proxy::{InvalidOutboundProxy, OutboundProxy};
pub mod scope;
mod selection;
mod smart_scheduling;
mod store;

pub use error::CredentialError;
pub use model::*;
pub(crate) use selection::smart_score;
pub use selection::*;
pub use smart_scheduling::{SmartSchedulingConfig, SmartSchedulingConfigError};
pub use store::ProviderAccountStore;
