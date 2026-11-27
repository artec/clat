use super::*;

// Goal reads need fresh account-specific data even if the foreground rail is on
// another profile. Keep credentials out of the process-wide cache.
type GoalQuotaSample = Arc<Mutex<Option<(Instant, ProviderAllowance)>>>;
static GOAL_QUOTA: Mutex<Vec<([u8; 32], GoalQuotaSample)>> = Mutex::new(Vec::new());

pub(super) fn read(
    config: &ModelConfig,
    credentials: &ProviderCredentials,
    force: bool,
) -> ProviderAllowance {
    use sha2::{Digest, Sha256};
    let Some(provider) = crate::providers::goal_direct_allowance_provider(config) else {
        return ProviderAllowance::state("unsupported");
    };
    let origin = url::Url::parse(&config.endpoint)
        .expect("validated endpoint")
        .origin()
        .ascii_serialization();
    let Some(key) = credentials
        .value(0)
        .filter(|value| !value.trim().is_empty())
    else {
        return ProviderAllowance::state("unavailable");
    };
    let hash: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&(provider, origin, key.trim())).expect("serializable identity"),
    )
    .into();
    let slot = {
        let mut cache = GOAL_QUOTA.lock().expect("goal allowance cache");
        if let Some((_, slot)) = cache.iter().find(|(id, _)| id == &hash) {
            slot.clone()
        } else {
            if cache.len() >= 64 {
                cache.remove(0);
            }
            let slot = Arc::new(Mutex::new(None));
            cache.push((hash, slot.clone()));
            slot
        }
    };
    let started = Instant::now();
    let mut sample = slot.lock().expect("goal allowance sample");
    if let Some((at, value)) = sample.as_ref()
        && (if force {
            *at >= started
        } else {
            at.elapsed() < Duration::from_secs(60)
        })
    {
        return value.clone();
    }
    let value = fetch_provider_allowance(config, key.trim());
    *sample = Some((Instant::now(), value.clone()));
    value
}
