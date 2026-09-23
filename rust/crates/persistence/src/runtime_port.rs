use crate::{CanonicalState, LifetimeLease, StateStore};
use contract::ErrorCode;
use domain::DomainError;
use runtime::{PersistencePort, RuntimeState};
use std::sync::{Arc, Mutex};

/// How many artifact publications a single Runtime commit rebases across before it reports the
/// conflict; each rebase needs another publication to land inside one commit.
const REBASE_ATTEMPTS: usize = 8;

#[derive(Clone)]
pub struct JsonPersistencePort {
    store: Arc<StateStore>,
    lease: Arc<LifetimeLease>,
    /// The Runtime-owned part of the state as last loaded, and the revision it was loaded at.
    loaded: Arc<Mutex<Option<(u64, CanonicalState)>>>,
}

impl JsonPersistencePort {
    pub fn new(store: Arc<StateStore>, lease: Arc<LifetimeLease>) -> Self {
        Self {
            store,
            lease,
            loaded: Arc::new(Mutex::new(None)),
        }
    }
}

/// The state without the parts the Runtime core does not own: the artifact manifest, which the
/// artifact store commits under its own lock, and the revision those commits advance.
fn runtime_owned(state: &CanonicalState) -> CanonicalState {
    CanonicalState {
        store_revision: 0,
        artifact_manifest: Vec::new(),
        ..state.clone()
    }
}

impl PersistencePort for JsonPersistencePort {
    fn load(&self) -> Result<RuntimeState, DomainError> {
        let (canonical, encoded_len) = self.store.load_measured(&self.lease)?;
        if let Ok(mut loaded) = self.loaded.lock() {
            *loaded = Some((canonical.store_revision, runtime_owned(&canonical)));
        }
        let mut state = RuntimeState::try_from(canonical)?;
        state.used_bytes = encoded_len;
        Ok(state)
    }

    fn compare_and_commit(
        &self,
        expected_revision: u64,
        candidate: RuntimeState,
    ) -> Result<(), DomainError> {
        if candidate.revision
            != expected_revision.checked_add(1).ok_or_else(|| {
                DomainError::new(ErrorCode::ResourceLimit, "store revision exhausted")
            })?
        {
            return Err(DomainError::new(
                ErrorCode::RevisionConflict,
                "candidate revision is not the next revision",
            ));
        }
        let replacement = CanonicalState::try_from(&candidate)?;
        // The artifact store publishes and evicts under its own lock, so a capture that settles
        // while this commit is prepared advances the revision without touching anything this
        // commit owns. The commit rebases across such publications; any other change is a conflict.
        let loaded = self
            .loaded
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "store view lock failed"))?
            .clone()
            .filter(|(revision, _)| *revision == expected_revision)
            .map(|(_, view)| view);
        let mut revision = expected_revision;
        for _ in 0..REBASE_ATTEMPTS {
            let mut attempt = replacement.clone();
            attempt.store_revision = revision;
            let rebased = revision != expected_revision;
            let view = loaded.clone();
            let committed = self
                .store
                .compare_and_commit(&self.lease, revision, move |state| {
                    if rebased && view.as_ref() != Some(&runtime_owned(state)) {
                        return Err(DomainError::new(
                            ErrorCode::RevisionConflict,
                            "the Runtime-owned state changed since it was loaded",
                        ));
                    }
                    attempt.artifact_manifest = state.artifact_manifest.clone();
                    *state = attempt;
                    Ok(())
                });
            match committed {
                Ok(_) => return Ok(()),
                Err(error) if error.code == ErrorCode::RevisionConflict && loaded.is_some() => {
                    let current = self.store.load(&self.lease)?;
                    if current.store_revision == revision
                        || loaded.as_ref() != Some(&runtime_owned(&current))
                    {
                        return Err(error);
                    }
                    revision = current.store_revision;
                }
                Err(error) => return Err(error),
            }
        }
        Err(DomainError::new(
            ErrorCode::RevisionConflict,
            "artifact publications kept advancing the store revision",
        ))
    }
}
