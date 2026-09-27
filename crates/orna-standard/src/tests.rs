mod v1_profile;

use super::{
    STANDARD_LIBRARY_REVISION_ID, STANDARD_LIBRARY_V2_REVISION_ID,
    STANDARD_LIBRARY_V3_REVISION_ID, STANDARD_LIBRARY_V4_REVISION_ID,
    STANDARD_LIBRARY_V5_REVISION_ID, STANDARD_LIBRARY_V6_REVISION_ID,
    STANDARD_LIBRARY_V7_REVISION_ID, STANDARD_LIBRARY_V8_REVISION_ID,
    STANDARD_LIBRARY_V9_REVISION_ID, STANDARD_LIBRARY_V10_REVISION_ID,
    StandardLibraryError, select_verified_standard_library,
};

#[test]
fn retired_historical_standard_revisions_fail_closed() {
    for revision in [
        STANDARD_LIBRARY_REVISION_ID,
        STANDARD_LIBRARY_V2_REVISION_ID,
        STANDARD_LIBRARY_V3_REVISION_ID,
        STANDARD_LIBRARY_V4_REVISION_ID,
        STANDARD_LIBRARY_V5_REVISION_ID,
        STANDARD_LIBRARY_V6_REVISION_ID,
        STANDARD_LIBRARY_V7_REVISION_ID,
        STANDARD_LIBRARY_V8_REVISION_ID,
        STANDARD_LIBRARY_V9_REVISION_ID,
        STANDARD_LIBRARY_V10_REVISION_ID,
    ] {
        assert!(matches!(
            select_verified_standard_library(revision),
            Err(StandardLibraryError::UnsupportedRevision { revision: actual })
                if actual == revision
        ));
    }
}
