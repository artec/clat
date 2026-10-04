use super::*;
pub(super) fn owner_error(error: Error) -> Failure {
    match error {
        Error::Authority(error) => dns_error(error),
        Error::Limit => Failure::LimitExceeded,
        Error::InvalidHandle => Failure::InvalidResolution,
        Error::Table => Failure::TransportFailed,
    }
}
pub(super) fn dns_error(error: crate::dns_authority::Failure) -> Failure {
    use crate::dns_authority::Failure as E;
    match error {
        E::BlockedAddress | E::InvalidDiscovery => Failure::BlockedAddress,
        E::LimitExceeded => Failure::LimitExceeded,
        E::ResolutionFailed => Failure::NameResolutionFailed,
        E::Cancelled => Failure::Cancelled,
        E::DeadlineExceeded => Failure::DeadlineExceeded,
        E::InvalidResolution => Failure::InvalidResolution,
        E::InvalidOrigin => Failure::InvalidRequest,
        E::CapabilityDenied => Failure::CapabilityDenied,
    }
}
pub(super) fn permission_error(error: crate::http_authority::permission::Failure) -> Failure {
    use crate::http_authority::permission::Failure as E;
    match error {
        E::Denied => Failure::PermissionDenied,
        E::Cancelled => Failure::Cancelled,
        E::Deadline => Failure::DeadlineExceeded,
        E::Busy => Failure::LimitExceeded,
        E::Unavailable => Failure::CapabilityDenied,
        E::InvalidRequest => Failure::InvalidRequest,
    }
}
pub(super) fn http_error(error: crate::http_authority::transport::Failure) -> Failure {
    use crate::http_authority::transport::Failure as E;
    match error {
        E::Authority(error) => dns_error(error),
        E::Permission(error) => permission_error(error),
        E::Transport => Failure::TransportFailed,
        E::Limit => Failure::LimitExceeded,
        E::Encoding => Failure::Unsupported,
    }
}
pub(super) fn task_error(error: task::Error) -> Failure {
    match error {
        task::Error::Authority(error) => dns_error(error),
        task::Error::Http(error) => http_error(error),
        task::Error::Cancelled => Failure::Cancelled,
        task::Error::Consumed => Failure::Consumed,
    }
}
pub(super) fn request_error(error: crate::http_authority::Failure) -> Failure {
    use crate::http_authority::Failure as E;
    match error {
        E::InvalidRequest => Failure::InvalidRequest,
        E::LimitExceeded => Failure::LimitExceeded,
        E::CapabilityDenied => Failure::CapabilityDenied,
    }
}
