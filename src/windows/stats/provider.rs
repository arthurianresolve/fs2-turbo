#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FallbackReason {
    ProviderMissing,
    ProviderUnavailable,
    VolumeResolution,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderOutcome<T> {
    Value(T),
    Unavailable(FallbackReason),
}
