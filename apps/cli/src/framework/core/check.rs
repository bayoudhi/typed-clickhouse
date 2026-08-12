use crate::project::LanguageProjectConfig;
use async_trait::async_trait;

use crate::framework::typescript::checker::TypeScriptChecker;

#[derive(Debug, thiserror::Error)]
pub enum CheckerError {
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Not supported: {0}")]
    NotSupported(String),
}

#[async_trait]
pub trait SystemChecker {
    async fn validate(&self) -> Result<(), CheckerError>;
}

pub async fn check_system_reqs(config: &LanguageProjectConfig) -> Result<(), CheckerError> {
    match config {
        LanguageProjectConfig::Typescript(_) => {
            let checker = TypeScriptChecker;
            checker.validate().await
        }
    }
}
