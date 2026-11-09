use crate::*;

impl Repository {
    /// Atomically persists the current publication journal in private runtime
    /// state. The ordinary Git namespace is not touched.
    pub fn write_publication_journal(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.write_publication_journal_locked(journal)
    }

    pub(crate) fn write_publication_journal_locked(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        self.require_no_pending_checkout_recovery_locked()?;
        let encoded = journal.encode()?;
        self.runtime.ensure_exists()?;
        let path = self.runtime.root().join("publication-journal.bin");
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && metadata.file_type().is_symlink()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let temporary = self.runtime.root().join(format!(
            ".publication-journal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?
                .as_nanos()
        ));
        write_atomically(&path, &temporary, &encoded)
    }

    /// Reads the last atomically persisted publication journal, if present.
    pub fn read_publication_journal(&self) -> Result<Option<PublicationJournal>, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.read_publication_journal_locked()
    }

    /// Reports whether a persisted journal belongs to a reviewed commit. The
    /// review anchor naming the candidate is pinned before any journal is
    /// written, so a journal whose intent has an anchor at its candidate is
    /// completed by `commit_reviewed_candidate` and never by a runtime freeze.
    pub fn publication_journal_is_reviewed(
        &self,
        journal: &PublicationJournal,
    ) -> Result<bool, RepositoryError> {
        let Some(intent) = journal.runtime_intent_id() else {
            return Ok(false);
        };
        let anchor = format!(
            "refs/orna/review-candidates/{}",
            intent
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let pinned = self.commit_optional(&format!("{anchor}^{{commit}}"))?;
        Ok(pinned.is_some_and(|commit| commit.as_str() == journal.new_head().as_str()))
    }

    pub(crate) fn read_publication_journal_locked(
        &self,
    ) -> Result<Option<PublicationJournal>, RepositoryError> {
        let path = self.runtime.root().join("publication-journal.bin");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let bytes = fs::read(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(Some(PublicationJournal::decode(
            &bytes,
            self.native_object_id_length()?,
        )?))
    }

    /// Removes a completed or abandoned journal idempotently and flushes the
    /// containing private runtime directory.
    pub fn clear_publication_journal(&self) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.clear_publication_journal_locked()
    }

    pub(crate) fn clear_publication_journal_locked(&self) -> Result<(), RepositoryError> {
        self.require_no_pending_checkout_recovery_locked()?;
        let path = self.runtime.root().join("publication-journal.bin");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            Ok(_) => {
                fs::remove_file(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                fs::File::open(self.runtime.root())
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        }
        self.sweep_publication_pins_locked(&[])?;
        Ok(())
    }

    /// Pins the objects an unfinished publication journal needs for recovery
    /// (ORNA-GIT-010): its base index tree and its candidate commit. The pins
    /// are private and owned by the journal. They are adopted before Git's
    /// index lock is taken or the Prepared journal is written, and every pin
    /// that no journal names is swept, so GC and repack cannot prune the
    /// objects reconciliation reads.
    pub(crate) fn adopt_publication_pins_locked(
        &self,
        journal: &PublicationJournal,
        policy: PublicationPinPolicy,
    ) -> Result<(), RepositoryError> {
        let pins = Self::publication_pins(journal);
        let keep: Vec<String> = pins
            .iter()
            .map(|(reference, _)| reference.clone())
            .collect();
        self.sweep_publication_pins_locked(&keep)?;
        for (reference, oid) in &pins {
            self.pin_publication_object_locked(reference, oid, policy)?;
        }
        Ok(())
    }

    /// The pin refs and the objects they keep for one journal.
    pub(crate) fn publication_pins(journal: &PublicationJournal) -> Vec<(String, String)> {
        let mut pins = Vec::new();
        if let Some(tree) = journal.base_index_tree.as_ref() {
            pins.push((
                Self::publication_index_pin_ref(tree.as_str()),
                tree.as_str().to_owned(),
            ));
        }
        pins.push((
            Self::publication_candidate_pin_ref(journal.new_head.as_str()),
            journal.new_head.as_str().to_owned(),
        ));
        pins
    }

    /// Releases the pins of a failed publication attempt. A retained journal
    /// keeps its own pins; with no journal every publication pin is swept.
    /// Acquires the coordination lock, so callers must not hold it.
    pub(crate) fn release_unjournaled_publication_pins(&self) -> Result<(), RepositoryError> {
        let _coordination_lock = self.acquire_coordination_lock()?;
        let keep: Vec<String> = match self.read_publication_journal_locked()? {
            Some(journal) => Self::publication_pins(&journal)
                .into_iter()
                .map(|(reference, _)| reference)
                .collect(),
            None => Vec::new(),
        };
        self.sweep_publication_pins_locked(&keep)
    }

    pub(crate) fn publication_index_pin_ref(tree: &str) -> String {
        format!("refs/orna/publication-indexes/{tree}")
    }

    pub(crate) fn publication_candidate_pin_ref(commit: &str) -> String {
        format!("refs/orna/publication-candidates/{commit}")
    }

    /// Pins one object a journal names. Recovery (`AcceptLostObjects`) leaves
    /// an object Git no longer holds unpinned: the recovery path already
    /// classifies a lost candidate or base tree (ORNA-GIT-010). Publish
    /// (`RequireObjects`) refuses instead, so a prepared candidate or base is
    /// never published while its object is unpinned (ORNA-CAP-006).
    pub(crate) fn pin_publication_object_locked(
        &self,
        reference: &str,
        oid: &str,
        policy: PublicationPinPolicy,
    ) -> Result<(), RepositoryError> {
        if !self.publication_object_present(oid)? {
            return policy.absent_object();
        }
        let zero = "0".repeat(oid.len());
        let mut update = self.command();
        update.args(["update-ref", "--no-deref", reference, oid, zero.as_str()]);
        if self.run(update).is_ok() {
            return Ok(());
        }
        // A GC that removed the object after the check above makes the create
        // fail. That is the same absent-object case.
        if !self.publication_object_present(oid)? {
            return policy.absent_object();
        }
        // Creation is refused when the pin already names this object. Accept
        // only that exact pin; any other value is a conflicting journal pin.
        let mut verify = self.command();
        verify.args(["rev-parse", "--verify", "--quiet", reference]);
        let output = verify
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() && trim_output(&output.stdout) == oid {
            Ok(())
        } else {
            Err(RepositoryError::InvalidPublicationJournal)
        }
    }

    /// `git cat-file -e` exits 0 when the object exists and 1 when it is
    /// absent. Any other status (a fatal repository error, a corrupt object
    /// store) is an error: reading it as "absent" would skip the pin while
    /// the journal naming the object is still written.
    pub(crate) fn publication_object_present(&self, oid: &str) -> Result<bool, RepositoryError> {
        let mut exists = self.command();
        exists.args(["cat-file", "-e", oid]);
        let output = exists
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(RepositoryError::GitOperationFailed),
        }
    }

    /// Deletes every publication pin not named in `keep`. Callers hold the
    /// coordination lock, so no publisher can sit between its pin and its
    /// journal. With no journal no pin may survive; with a journal only its
    /// own pins may (ORNA-GIT-010).
    pub(crate) fn sweep_publication_pins_locked(
        &self,
        keep: &[String],
    ) -> Result<(), RepositoryError> {
        let mut list = self.command();
        list.args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/orna/publication-indexes/",
            "refs/orna/publication-candidates/",
        ]);
        let listing = self.run(list)?.stdout;
        for line in listing
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let reference =
                std::str::from_utf8(line).map_err(|_| RepositoryError::GitOperationFailed)?;
            if keep.iter().any(|kept| kept == reference) {
                continue;
            }
            let mut delete = self.command();
            delete.args(["update-ref", "-d", "--no-deref", reference]);
            self.run(delete)?;
        }
        Ok(())
    }

    pub(crate) fn reclaim_abandoned_publication_index_lock(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let current = self
            .read_publication_journal_locked()?
            .ok_or(RepositoryError::GitIndexLockPresent)?;
        if &current != journal {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let index = self.git_path("index")?;
        GitIndexLock::reclaim_abandoned(index.with_extension("lock"), journal.lock_binding()?)
    }

    /// Reclaims an Orna-marked `index.lock` left by a publisher that stopped
    /// after taking Git's lock but before writing its Prepared journal. No
    /// journal binds such a lock, so the Orna marker must carry a publisher
    /// binding (not the all-zero ordinary-writer binding) and the owner must be
    /// proven dead. Any other lock is left in place for Git and does not fail
    /// recovery; the next publisher reports it as present.
    pub(crate) fn reclaim_orphan_publication_index_lock(&self) -> Result<(), RepositoryError> {
        let index = self.git_path("index")?;
        match GitIndexLock::reclaim_unbound_orphan(index.with_extension("lock")) {
            Err(RepositoryError::GitIndexLockPresent) => Ok(()),
            result => result,
        }
    }

    pub(crate) fn write_checkout_recovery_journal_locked(
        &self,
        journal: &CheckoutRecoveryJournal,
    ) -> Result<(), RepositoryError> {
        self.require_no_pending_publication_recovery_locked()?;
        let encoded = journal.encode()?;
        self.runtime.ensure_exists()?;
        let path = self.runtime.root().join("checkout-journal.bin");
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && metadata.file_type().is_symlink()
        {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let temporary = self.runtime.root().join(format!(
            ".checkout-journal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?
                .as_nanos()
        ));
        write_atomically(&path, &temporary, &encoded)
    }

    pub(crate) fn read_checkout_recovery_journal_locked(
        &self,
    ) -> Result<Option<CheckoutRecoveryJournal>, RepositoryError> {
        let path = self.runtime.root().join("checkout-journal.bin");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let bytes = fs::read(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(Some(CheckoutRecoveryJournal::decode(
            &bytes,
            self.native_object_id_length()?,
        )?))
    }

    pub(crate) fn clear_checkout_recovery_journal_locked(&self) -> Result<(), RepositoryError> {
        self.require_no_pending_publication_recovery_locked()?;
        let path = self.runtime.root().join("checkout-journal.bin");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(RepositoryError::InvalidCheckoutJournal);
            }
            Ok(_) => {
                fs::remove_file(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                fs::File::open(self.runtime.root())
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        }
        Ok(())
    }
}
