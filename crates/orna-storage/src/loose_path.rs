use orna_repository::ManagedPath;
use orna_value::{
    path_decode_key_components, path_encode_key_components, path_validate_relative_components,
};

use crate::Error;

/// A validated legacy loose-row path derived with the v1 path codec.
/// Format-1/2 snapshots address rows by this path; format-3 rows have none.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LoosePath(ManagedPath);

impl LoosePath {
    /// Admits a discovered table-relative encoded row path. The components
    /// exclude the table root and include the final `.orna` extension.
    /// Decoding rejects aliases before construction; this does not perform
    /// filesystem I/O, resolve symlinks, or interpret the key's logical type.
    pub fn from_encoded_key(
        table_root: impl AsRef<str>,
        components: &[String],
    ) -> Result<Self, Error> {
        let key = path_decode_key_components(components).map_err(|_| Error::InvalidKey)?;
        Self::for_key(table_root, &key)
    }

    /// `key` contains canonical text leaves in declared order. The catalogue
    /// flattens tuples and table references to those leaves; storage owns only
    /// their portable path encoding and does not embed referenced rows.
    pub fn for_key(table_root: impl AsRef<str>, key: &[String]) -> Result<Self, Error> {
        let root = table_root.as_ref();
        if root.is_empty() || root.starts_with('.') || root.contains('/') || root.contains('\\') {
            return Err(Error::InvalidTableRoot);
        }
        let components = path_encode_key_components(key).map_err(|_| Error::InvalidKey)?;
        // Logical keys may contain separators, traversal spellings, or empty
        // strings. Only their encoded filesystem components require this check.
        path_validate_relative_components(&components).map_err(|_| Error::InvalidKey)?;
        let path = format!("{root}/{}", components.join("/"));
        ManagedPath::new(path)
            .map(Self)
            .map_err(|_| Error::UnsafePath)
    }

    pub fn as_managed_path(&self) -> &ManagedPath {
        &self.0
    }
}

impl Ord for LoosePath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_path().cmp(other.0.as_path())
    }
}

impl PartialOrd for LoosePath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_keys_are_encoded_before_path_safety_validation() {
        for (key, encoded) in [
            ("", "~ff"),
            (".", "~2e"),
            ("..", "~2e~2e"),
            ("foo/bar", "foo~2fbar"),
            ("foo\\bar", "foo~5cbar"),
            ("/outside", "~2foutside"),
            (".git", "~2egit"),
            ("con", "~63on"),
            ("é", "~c3~a9"),
        ] {
            let path = LoosePath::for_key("Contact", &[key.into()]).unwrap();
            assert_eq!(
                path.as_managed_path().as_path(),
                std::path::Path::new(&format!("Contact/{encoded}.orna"))
            );
            assert_eq!(orna_value::path_decode_component(encoded).unwrap(), key);
        }
        let composite = LoosePath::for_key("Contact", &["..".into(), "a/b".into()]).unwrap();
        assert_eq!(
            composite.as_managed_path().as_path(),
            std::path::Path::new("Contact/~2e~2e/a~2fb.orna")
        );
        assert!(LoosePath::for_key("Contact", &[]).is_err());
        assert!(LoosePath::for_key("Contact", &["a".repeat(201)]).is_err());
        assert!(LoosePath::for_key("Contact", &vec!["a".repeat(200); 6]).is_err());
        for root in ["", ".", "..", "../Contact", "/Contact", "a\\b"] {
            assert!(LoosePath::for_key(root, &["safe".into()]).is_err());
        }
    }

    #[test]
    fn discovered_paths_must_be_canonical_before_admission() {
        for (keys, components) in [
            (vec![""], vec!["~ff.orna"]),
            (vec!["..", "foo/bar"], vec!["~2e~2e", "foo~2fbar.orna"]),
            (
                vec!["first.orna", "last.orna"],
                vec!["first.orna", "last.orna.orna"],
            ),
        ] {
            let keys: Vec<String> = keys.into_iter().map(String::from).collect();
            let components: Vec<String> = components.into_iter().map(String::from).collect();
            let discovered = LoosePath::from_encoded_key("Contact", &components).unwrap();
            assert_eq!(discovered, LoosePath::for_key("Contact", &keys).unwrap());
            assert_eq!(
                discovered.as_managed_path().as_path(),
                std::path::Path::new(&format!("Contact/{}", components.join("/")))
            );
        }
        for components in [
            vec![],
            vec!["alice"],
            vec!["~61lice.orna"],
            vec!["~FF.orna"],
            vec!["..", "alice.orna"],
            vec!["/alice.orna"],
            vec!["a\\alice.orna"],
        ] {
            let components: Vec<String> = components.into_iter().map(String::from).collect();
            assert_eq!(
                LoosePath::from_encoded_key("Contact", &components),
                Err(Error::InvalidKey)
            );
        }
        assert!(LoosePath::from_encoded_key("../Contact", &["alice.orna".into()]).is_err());
    }
}
