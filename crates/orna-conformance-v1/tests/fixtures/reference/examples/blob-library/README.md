# Binary library fixture

`main.orna` imports `music.orna`; no optional library is required. The 1.1 core supplies the exact Blob operations. A test runner creates a disposable format-2 Git database, imports the generated `tone.wav` and `pixel.png` under two distinct keys using `music.import_song`, and then calls `music.length_of`/`music.first_bytes`. The table is deliberately generic enough for both payloads: a production library may name it differently or add metadata.

The source media are produced by `python tools/make_media_fixtures.py` at the specification root. These are original synthetic test inputs, not user media. The runner passes their temporary parent directory as the explicitly authorised root. No host absolute path is hardcoded into source. Expected sizes/digests are in `tests/media-fixtures.json`.

This is an authored engine fixture; it is not a claim that the currently installed engine implements the new API. After implementation, the runner verifies metadata edits do not read payloads; source deletion does not break rows; old Git revisions return old bytes; partial range reads hydrate only necessary chunks; complete offline copying includes dependencies. No Watch or saved-snippet feature is needed.
