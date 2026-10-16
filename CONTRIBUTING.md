# Developer Workflow

### Testing
1. Run `./build.sh`.
2. Select your distro.
3. Use the Git Hash (displayed in the script) to track your testing version.

### Releasing
1. Bump the version in `Cargo.toml`.
2. Run `cargo update` to sync the lock file.
3. Tag the commit: `git tag -a v1.x.x -m "Release message"`.
4. Run `./build.sh` to generate the final packages.

### Translations
The interface language follows the system locale (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`), falling back to English. Bundled languages: English, Português (Brasil), Español.

To test a language without changing your system locale:
```bash
ANDROID_WEBCAM_LANG=pt_BR cargo run
```

To add a new language:
1. Copy `locales/en.json` to `locales/<code>.json`, using the locale code from `LANG` without the encoding (e.g. `fr`, `de`, `pt_PT`). A bare language code like `fr` also covers every region (`fr_FR`, `fr_CA`, ...).
2. Translate the values only. Keep the keys and the `{placeholders}` exactly as they are.
3. Register the file in the `LOCALES` list in `src/i18n.rs`:
   ```rust
   ("fr", include_str!("../locales/fr.json")),
   ```
4. Run `cargo test`. It fails if your file is missing keys, has unknown keys, or changed a placeholder.
5. Optionally add `Comment[<code>]=` to `resources/android-webcam.desktop`.

When adding new UI text, put the English string in `locales/en.json` and use `tr("key")` / `trf("key", &[("name", value)])` in the code. Missing keys in other languages fall back to English.
