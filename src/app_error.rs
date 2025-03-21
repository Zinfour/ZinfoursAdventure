use {
    axum::{
        http::{header::InvalidHeaderValue, StatusCode},
        response::{IntoResponse, Response},
    },
    rand::seq::WeightError,
    redb::{CommitError, StorageError, TableError, TransactionError},
    std::time::SystemTimeError,
    tokio::task::JoinError,
};

pub enum AppError {
    StringError(String),
    TransactionError(TransactionError),
    StorageError(StorageError),
    TableError(TableError),
    JoinError(JoinError),
    CommitError(CommitError),
    SystemTimeError(SystemTimeError),
    InvalidHeaderValue(InvalidHeaderValue),
    WeightError(WeightError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // How we want errors responses to be serialized
        let (status, message): (StatusCode, String) = match self {
            AppError::StringError(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::TransactionError(transaction_error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                transaction_error.to_string(),
            ),
            AppError::StorageError(storage_error) => {
                (StatusCode::INTERNAL_SERVER_ERROR, storage_error.to_string())
            }
            AppError::TableError(table_error) => {
                (StatusCode::INTERNAL_SERVER_ERROR, table_error.to_string())
            }
            AppError::JoinError(join_error) => {
                (StatusCode::INTERNAL_SERVER_ERROR, join_error.to_string())
            }
            AppError::CommitError(commit_error) => {
                (StatusCode::INTERNAL_SERVER_ERROR, commit_error.to_string())
            }
            AppError::SystemTimeError(systemtime_error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                systemtime_error.to_string(),
            ),
            AppError::InvalidHeaderValue(invalidheader_error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                invalidheader_error.to_string(),
            ),
            AppError::WeightError(weighterror_error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                weighterror_error.to_string(),
            ),
        };

        (status, message).into_response()
    }
}

impl From<&str> for AppError {
    fn from(msg: &str) -> Self {
        AppError::StringError(msg.to_string())
    }
}

impl From<String> for AppError {
    fn from(msg: String) -> Self {
        AppError::StringError(msg)
    }
}

impl From<TransactionError> for AppError {
    fn from(transaction_err: TransactionError) -> Self {
        AppError::TransactionError(transaction_err)
    }
}

impl From<StorageError> for AppError {
    fn from(storage_err: StorageError) -> Self {
        AppError::StorageError(storage_err)
    }
}

impl From<TableError> for AppError {
    fn from(table_err: TableError) -> Self {
        AppError::TableError(table_err)
    }
}

impl From<JoinError> for AppError {
    fn from(join_err: JoinError) -> Self {
        AppError::JoinError(join_err)
    }
}

impl From<CommitError> for AppError {
    fn from(commit_err: CommitError) -> Self {
        AppError::CommitError(commit_err)
    }
}

impl From<SystemTimeError> for AppError {
    fn from(systemtime_err: SystemTimeError) -> Self {
        AppError::SystemTimeError(systemtime_err)
    }
}

impl From<InvalidHeaderValue> for AppError {
    fn from(invalidheader_err: InvalidHeaderValue) -> Self {
        AppError::InvalidHeaderValue(invalidheader_err)
    }
}

impl From<WeightError> for AppError {
    fn from(weighterror_err: WeightError) -> Self {
        AppError::WeightError(weighterror_err)
    }
}
