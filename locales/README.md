# Translations

English UI messages are translation keys. `sv.json` and `de.json` contain Swedish
and German translations, embedded in the binary at build time; no installed locale
package is required for gsnag's own messages. GTK's standard dialogs follow the
desktop locale. Technical CLI help and underlying library errors remain English.

To add a language, copy a catalog, translate every value (preserving all `{named}`
placeholders), register its code/name and `include_str!` in `gsnag-i18n/src/lib.rs`,
and add the code to the CLI language parsers. Run `make test` and
`python3 tests/locales.py`. Missing messages fall back to English.

Selection order: `--language` for this launch, `GSNAG_LANGUAGE`, saved preference,
then `LC_ALL`, `LC_MESSAGES`, `LANG`, finally English. Regional variants such as
`sv_SE.UTF-8` and `de-DE` are accepted by environment detection. The saved preference
is `$XDG_CONFIG_HOME/gsnag/language` (normally `~/.config/gsnag/language`).

The tray's Language menu applies to newly opened windows and updates the menu
immediately. Existing editor/recording windows retain their launch language.
Use `gsnag language auto` to return to system detection. An explicit environment
or command-line override takes precedence over the saved setting on the next launch.
