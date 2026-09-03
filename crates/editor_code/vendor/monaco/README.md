# Vendored Monaco Editor

`vs/` is the **trimmed `min/vs`** build of [`monaco-editor`], served
offline to the code-editor webview via the `monaco://` custom protocol
(see `../../src/lib.rs`). `index.html` is our own boot page.

- **Version:** `monaco-editor@0.56.0`
- **Trim:** removed `vs/language/` (TS/CSS/HTML/JSON language *services*),
  `vs/nls/lang/` (non-English translations), and the `ts/css/html/json`
  worker bundles under `vs/assets/`. **Kept** the core editor worker
  (`vs/assets/editor.worker-*.js`, `editorWebWorkerMain-*.js`) — Monaco's
  bundle fetches it during init even with workers disabled, and a 404
  there aborts editor creation. Also kept every syntax grammar and the
  codicon font. ~5.5 MB.

## Refreshing

```sh
npm pack monaco-editor@<version>
tar -xzf monaco-editor-<version>.tgz
rm -rf vs && cp -R package/min/vs vs
rm -rf vs/language vs/nls/lang
rm -f vs/assets/ts.worker* vs/assets/css.worker* vs/assets/html.worker* vs/assets/json.worker*
```

Then bump the version above and re-run the editor to sanity-check.

## Notes

- Web workers are disabled in `index.html` (`MonacoEnvironment.getWorker`
  returns a no-op). Highlighting, bracket matching, and editing work
  synchronously; rich IntelliSense would need the workers back and is
  the job of the LSP bridge task anyway.
- The `min/vs` files use content-hashed names internally; we never
  reference a hash from Rust, so a refresh that changes hashes needs no
  code change.

[`monaco-editor`]: https://www.npmjs.com/package/monaco-editor
