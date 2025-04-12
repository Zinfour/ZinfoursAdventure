use {
    crate::AppState,
    axum::{
        http::{header::InvalidHeaderValue, StatusCode},
        response::{IntoResponse, Response},
    },
    rand::seq::WeightError,
    redb::{CommitError, StorageError, TableError, TransactionError},
    std::{fmt::Display, time::SystemTimeError},
    tokio::task::JoinError,
    tower_sessions::session,
};

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    InternalServerError(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // How we want errors responses to be serialized
        let (status, message): (StatusCode, String) = match self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::InternalServerError(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            // AppError::TransactionError(transaction_error) => (
            //     StatusCode::INTERNAL_SERVER_ERROR,
            //     transaction_error.to_string(),
            // ),
            // AppError::StorageError(storage_error) => {
            //     (StatusCode::INTERNAL_SERVER_ERROR, storage_error.to_string())
            // }
            // AppError::TableError(table_error) => {
            //     (StatusCode::INTERNAL_SERVER_ERROR, table_error.to_string())
            // }
            // AppError::JoinError(join_error) => {
            //     (StatusCode::INTERNAL_SERVER_ERROR, join_error.to_string())
            // }
            // AppError::CommitError(commit_error) => {
            //     (StatusCode::INTERNAL_SERVER_ERROR, commit_error.to_string())
            // }
            // AppError::SystemTimeError(systemtime_error) => (
            //     StatusCode::INTERNAL_SERVER_ERROR,
            //     systemtime_error.to_string(),
            // ),
            // AppError::InvalidHeaderValue(invalidheader_error) => (
            //     StatusCode::INTERNAL_SERVER_ERROR,
            //     invalidheader_error.to_string(),
            // ),
            // AppError::WeightError(weighterror_error) => (
            //     StatusCode::INTERNAL_SERVER_ERROR,
            //     weighterror_error.to_string(),
            // ),
            // AppError::SessionError(session_error) => {
            //     (StatusCode::INTERNAL_SERVER_ERROR, session_error.to_string())
            // }
        };

        (status, message).into_response()
    }
}

impl From<&str> for AppError {
    fn from(msg: &str) -> Self {
        AppError::InternalServerError(msg.to_string())
    }
}

impl From<String> for AppError {
    fn from(msg: String) -> Self {
        AppError::InternalServerError(msg)
    }
}

impl From<TransactionError> for AppError {
    fn from(transaction_err: TransactionError) -> Self {
        AppError::InternalServerError(transaction_err.to_string())
    }
}

impl From<StorageError> for AppError {
    fn from(storage_err: StorageError) -> Self {
        AppError::InternalServerError(storage_err.to_string())
    }
}

impl From<TableError> for AppError {
    fn from(table_err: TableError) -> Self {
        AppError::InternalServerError(table_err.to_string())
    }
}

impl From<JoinError> for AppError {
    fn from(join_err: JoinError) -> Self {
        AppError::InternalServerError(join_err.to_string())
    }
}

impl From<CommitError> for AppError {
    fn from(commit_err: CommitError) -> Self {
        AppError::InternalServerError(commit_err.to_string())
    }
}

impl From<SystemTimeError> for AppError {
    fn from(systemtime_err: SystemTimeError) -> Self {
        AppError::InternalServerError(systemtime_err.to_string())
    }
}

impl From<InvalidHeaderValue> for AppError {
    fn from(invalidheader_err: InvalidHeaderValue) -> Self {
        AppError::InternalServerError(invalidheader_err.to_string())
    }
}

impl From<WeightError> for AppError {
    fn from(weighterror_err: WeightError) -> Self {
        AppError::InternalServerError(weighterror_err.to_string())
    }
}

impl From<axum_login::Error<AppState>> for AppError {
    fn from(value: axum_login::Error<AppState>) -> Self {
        match value {
            axum_login::Error::Session(session_err) => {
                AppError::InternalServerError(session_err.to_string())
            }
            axum_login::Error::Backend(app_error) => app_error,
        }
    }
}

impl Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::BadRequest(err) => write!(f, "BAD REQUEST: {}", err),
            AppError::InternalServerError(err) => write!(f, "INTERNAL SERVER ERROR: {}", err),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}
