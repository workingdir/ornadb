//! Typed catalogue references as executor values.
//!
//! A `sys.XRef` of any canonical relation (`sys.FunctionRef`, `sys.ObjectRef`, `sys.BranchRef`,
//! `sys.CommitRef` and the rest of `api/sys.json`) is a checked [`SysRef`]: a pinned row reference whose
//! relation identity and natural key the registry of [`orna_value::relations`] verified. Each one is a
//! [`Value::SysRef`], so a provider reads the typed reference through [`Value::as_sys_ref`] and the typed
//! accessors built on it instead of an opaque `Raw`. Its canonical bytes are the ones [`Value::to_raw`]
//! produces, so a stored reference reads back as an equal value.

use std::rc::Rc;

use orna_value::{
    CanonicalValue, FunctionRef, ObjectRef, Raw, RelationSpec, RowRef, SysRef, relations,
};

use super::Value;

impl Value {
    /// The executor value of a checked `sys.XRef`.
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-VALUE` when the canonical form of the reference does not encode.
    pub fn sys_ref(reference: SysRef) -> Result<Self, &'static str> {
        typed_row_raw(reference.as_row_ref())?;
        Ok(Self::SysRef(Rc::new(reference)))
    }

    /// The executor value of a checked `sys.FunctionRef`.
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-VALUE` when the reference is not on the `sys.Function` relation, its natural key is not a
    /// `sys.Function` key, or its canonical form does not encode.
    pub fn function_ref(reference: FunctionRef) -> Result<Self, &'static str> {
        Self::reference_of(reference.into_row_ref(), &relations::FUNCTION)
    }

    /// The executor value of a checked `sys.ObjectRef`.
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-VALUE` when the reference is not on the `sys.Object` relation, its natural key is not a
    /// `sys.Object` key, or its canonical form does not encode.
    pub fn object_ref(reference: ObjectRef) -> Result<Self, &'static str> {
        Self::reference_of(reference.into_row_ref(), &relations::OBJECT)
    }

    /// The reference `row` as a value, provided it is a valid reference to a row of `relation`.
    fn reference_of(row: RowRef, relation: &RelationSpec) -> Result<Self, &'static str> {
        let reference = SysRef::from_row_ref(row).map_err(|_| "ORNA-EVAL-VALUE")?;
        if !reference.is_of(relation) {
            return Err("ORNA-EVAL-VALUE");
        }
        Self::sys_ref(reference)
    }

    /// The `sys.XRef` this value holds, if it is one.
    pub fn as_sys_ref(&self) -> Option<&SysRef> {
        match self {
            Self::SysRef(reference) => Some(reference.as_ref()),
            _ => None,
        }
    }

    /// The reference this value holds when it is a reference to a row of `relation`: the typed read of a
    /// `sys.BranchRef` is `value.as_sys_ref_of(&relations::BRANCH)`.
    pub fn as_sys_ref_of(&self, relation: &RelationSpec) -> Option<&SysRef> {
        self.as_sys_ref()
            .filter(|reference| reference.is_of(relation))
    }

    /// The `sys.FunctionRef` this value holds, if it is one.
    pub fn as_function_ref(&self) -> Option<FunctionRef> {
        self.as_sys_ref_of(&relations::FUNCTION)
            .map(|reference| FunctionRef::from_row_ref(reference.as_row_ref().clone()))
    }

    /// The `sys.ObjectRef` this value holds, if it is one.
    pub fn as_object_ref(&self) -> Option<ObjectRef> {
        self.as_sys_ref_of(&relations::OBJECT)
            .map(|reference| ObjectRef::from_row_ref(reference.as_row_ref().clone()))
    }
}

/// The canonical raw form of a reference's row, which [`Value::to_raw`] returns.
pub(crate) fn typed_row_raw(row: &RowRef) -> Result<Raw, &'static str> {
    row.to_canonical()
        .map(|value| value.raw().clone())
        .map_err(|_| "ORNA-EVAL-VALUE")
}

/// The typed reference that the canonical raw value `raw` holds, or `None` when `raw` is not exactly the
/// canonical form of a `sys.XRef` of a canonical relation. The caller keeps `None` as an opaque
/// [`Value::RowRef`].
///
/// The relation identity selects the relation and the registry checks the natural key's shape, so a reference
/// to an unknown relation, or one whose key is not that relation's, stays opaque. The reference must re-encode
/// to `raw` byte for byte. A provider that dereferences the value still checks that the row exists and that
/// the caller may see it (ORNA-SYS-114).
pub(crate) fn typed_reference_from_raw(raw: &Raw) -> Option<Value> {
    let bytes = CanonicalValue::new(raw.clone()).ok()?.encode().ok()?;
    let row = RowRef::decode(&bytes).ok()?;
    let value = Value::SysRef(Rc::new(SysRef::from_row_ref(row).ok()?));
    (value.to_raw().ok()? == *raw).then_some(value)
}

/// Checked catalogue references shared by the tests of this crate.
#[cfg(test)]
pub(crate) mod samples {
    use orna_value::{
        CanonicalSnapshot, FunctionRef, GitHash, GitObjectId, ObjectRef, SysRef, branch_reference,
        commit_reference, function_reference, object_reference,
    };

    /// The `sys.ObjectRef` of catalogue object `[5; 16]` on database `[1; 16]`, pinned to a CWD snapshot.
    pub(crate) fn object_ref() -> ObjectRef {
        object_reference([1; 16], [5; 16], snapshot()).expect("object reference")
    }

    /// The `sys.FunctionRef` of [`object_ref`]: the same key and pin on the function relation.
    pub(crate) fn function_ref() -> FunctionRef {
        function_reference(object_ref()).expect("function reference")
    }

    /// The CWD snapshot the samples are pinned to.
    pub(crate) fn snapshot() -> CanonicalSnapshot {
        CanonicalSnapshot::cwd([1; 16], [2; 16], 3.into()).expect("snapshot")
    }

    /// The `sys.BranchRef` of branch `main`.
    pub(crate) fn branch_ref() -> SysRef {
        branch_reference([1; 16], snapshot(), "main").expect("branch reference")
    }

    /// The `sys.CommitRef` of a SHA-1 commit.
    pub(crate) fn commit_ref() -> SysRef {
        let oid = GitObjectId::new(GitHash::Sha1, vec![0xab; 20]).expect("object id");
        commit_reference([1; 16], snapshot(), oid).expect("commit reference")
    }
}

#[cfg(test)]
mod tests {
    use orna_value::{FunctionRef, ObjectRef, OvbRaw, RowRef, relations};

    use super::samples::{branch_ref, commit_ref, function_ref, object_ref, snapshot};
    use super::typed_reference_from_raw;
    use crate::raw_value::from_raw;
    use crate::value::Value;

    #[test]
    fn function_reference_is_a_typed_value_that_round_trips() {
        let reference = function_ref();
        let value = Value::function_ref(reference.clone()).expect("typed function reference");
        assert_eq!(value.as_function_ref(), Some(reference));
        assert_eq!(value.as_object_ref(), None);
        let raw = value.to_raw().expect("encodes");
        assert_eq!(from_raw(&raw).expect("reads back"), value);
    }

    #[test]
    fn object_reference_is_a_typed_value_that_round_trips() {
        let reference = object_ref();
        let value = Value::object_ref(reference.clone()).expect("typed object reference");
        assert_eq!(value.as_object_ref(), Some(reference));
        assert_eq!(value.as_function_ref(), None);
        let raw = value.to_raw().expect("encodes");
        assert_eq!(from_raw(&raw).expect("reads back"), value);
    }

    // Specified: ORNA-SYS-111, ORNA-SYS-125; Orna-1.1.0 sys.Branch and sys.Commit.
    #[test]
    fn branch_and_commit_references_are_typed_values_that_round_trip() {
        for (reference, relation, other) in [
            (branch_ref(), &relations::BRANCH, &relations::COMMIT),
            (commit_ref(), &relations::COMMIT, &relations::BRANCH),
        ] {
            let value = Value::sys_ref(reference.clone()).expect("typed reference");
            assert_eq!(value.as_sys_ref(), Some(&reference));
            assert_eq!(value.as_sys_ref_of(relation), Some(&reference));
            assert_eq!(value.as_sys_ref_of(other), None);
            assert_eq!(value.as_function_ref(), None);
            let raw = value.to_raw().expect("encodes");
            assert_eq!(from_raw(&raw).expect("reads back"), value);
        }
        // The two references share no representation, so neither reads back as the other.
        assert_ne!(
            Value::sys_ref(branch_ref()).unwrap(),
            Value::sys_ref(commit_ref()).unwrap()
        );
    }

    #[test]
    fn constructors_refuse_a_reference_on_another_relation() {
        assert_eq!(
            Value::object_ref(ObjectRef::from_row_ref(function_ref().into_row_ref())),
            Err("ORNA-EVAL-VALUE")
        );
        assert_eq!(
            Value::function_ref(FunctionRef::from_row_ref(object_ref().into_row_ref())),
            Err("ORNA-EVAL-VALUE")
        );
    }

    #[test]
    fn a_reference_on_an_unknown_relation_reads_back_opaque() {
        let object = object_ref().into_row_ref();
        let other =
            RowRef::new([1; 16], [9; 16], object.key, object.snapshot).expect("row reference");
        let raw = other.to_canonical().expect("canonical").raw().clone();
        assert!(typed_reference_from_raw(&raw).is_none());
        assert!(matches!(from_raw(&raw), Ok(Value::RowRef(_))));
    }

    #[test]
    fn a_reference_with_the_wrong_key_for_its_relation_reads_back_opaque() {
        let forged = RowRef::new(
            [1; 16],
            relations::BRANCH.table_id,
            OvbRaw::Bytes(vec![1, 2, 3]),
            snapshot(),
        )
        .expect("row reference");
        let raw = forged.to_canonical().expect("canonical").raw().clone();
        assert!(typed_reference_from_raw(&raw).is_none());
        assert!(matches!(from_raw(&raw), Ok(Value::RowRef(_))));
    }
}
