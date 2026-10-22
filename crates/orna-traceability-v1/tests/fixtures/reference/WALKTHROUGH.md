# One song, end to end

The language mutation spelling is retained. New mount/staging/capture behavior
below is the implementation target, not a terminal transcript from an engine.
The archive's evidence files clearly identify what was actually executed.

## 1. Source and values

`main.orna`:

```orna
pub table Music(id: Str) {
    title: Str,
    artist: Str,
    content: Blob,
    cover: Blob?,
}

pub fn rename(id: Str, title: Str) =
    Music.update(id, { title: title });

pub fn add(id: Str, title: Str, content: Blob) =
    Music.insert({
        id: id,
        title: title,
        artist: "Kieran",
        content: content,
        cover: null,
    });
```

Trusted REPL capture, using the specified shared APIs:

```orna
let raw = sys.blob.capture_file("/approved/imports", "recording.mp3", max_bytes: 20000000000);
let audio = sys.blob.annotate(raw, "audio/mpeg");
main.add("london", "Live in London", audio);
```

The path is an authorized source, not something the database retains as the
recording's identity. Successful capture/insertion protects the bytes in Git
objects and local durable state. No public Git commit is implicit.

## 2. Published physical tree

```text
library/
  main.orna
  .gitmodules                  # optional
  stdlib/std/                  # optional pinned dependency
  .orna/
    database.orna
    store/
      data
      refs/
        <child-native-oid>/
          data
          refs/...
```

The printed hierarchy is explanatory. `.orna/store` normally stays out of
sparse checkout. Git physically packs/shared-stores the objects; Orna does not
write every expanded graph path into ordinary directories. Application code
never addresses those node paths.

`database.orna`:

```orna
{
    repository_format: 3,
    database_id: "9f237b7e-6844-4498-bcd5-d24641c07449",
}
```

## 3. Virtual editable paths

```sh
orna --db ./library mount /mnt/library --cwd
cat /mnt/library/Music/london/data.orna
```

Expected projected data:

```orna
{
    title: "Live in London",
    artist: "Kieran",
    content: { path: "./content.mp3", media_type: "audio/mpeg" },
    cover: null,
}
```

```sh
mpv /mnt/library/Music/london/content.mp3
```

The player reads ordinary byte ranges; the shared Blob reader locates and
fetches necessary Git chunks. `ls`, `stat`, title reads and title edits fetch
no recording payload. Whole-object read amplification is measurable.

## 4. Same update, different interface

```orna
Music.update("london", { title: "Live in London — remastered" });
```

A subsequent `cat` shows the new title without a background export operation.
Editing the same field in `data.orna` submits a complete row candidate through
the same mutation engine. `title: 123` is rejected, the old row remains, and
the draft/diagnostic is recoverable. `content.mp3` stays untouched.

An ordinary editor supports the save patterns in VFS-1. Strong read-to-save
conflict protection is available through:

```sh
orna edit /mnt/library/Music/london/data.orna
```

Editing the MP3 replaces this row's Blob after successful sync/replacement;
old snapshots and other rows keep their original immutable values.

## 5. Review and publish separately

```sh
orna --db ./library add --row Music '"london"'
orna --db ./library status
orna --db ./library commit -m "Retitle the recording"
```

The row adapter stages the selected logical value in the actual index store.
Later unstaged edits remain separate. A devtools Commit button that promises a
reviewed candidate uses `sys.admin.review_commit` / `commit_reviewed`, not a
check-then-unprotected-commit sequence. Do not run `git add` inside the VFS.

## 6. Open the past

```sh
orna --db ./library mount /mnt/library-old --at HEAD~1
diff -u /mnt/library-old/Music/london/data.orna /mnt/library/Music/london/data.orna
```

Expected logical difference:

```diff
-    title: "Live in London",
+    title: "Live in London — remastered",
```

The old mount pins old code/schema/rows/content. `--at main` similarly resolves
main once rather than changing during a read. To work independently on another
branch, use an ordinary separate Git worktree and mount its CWD.

## 7. MIME ambiguity is explicit

```orna
let module = sys.blob.annotate(raw_script, "text/javascript", suffix: "mjs");
let old_mpeg = sys.blob.annotate(raw_audio, "audio/mpeg", suffix: "mp2");
```

MIME-1 prefers `.js` and `.mp3` absent those hints. Hints change row metadata,
never bytes or the raw-content SHA. `.gz` does not magically imply `.tar.gz`.
No host MIME database or hidden format sniff runs just to list a directory.
