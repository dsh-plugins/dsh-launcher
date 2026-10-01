# Pre-launch provider configuration (issue #76)

Instance Settings > **Providers** manages the model provider routes that DSH
reads through its `@deepseek-ai/dsh-llm-pi-ai` adapter.

## Where the configuration lives

Routes are written into a **Cordis patch layer**, not a settings file. Two scopes
are offered, mirroring the MCP tab:

| Scope | File |
|---|---|
| Global | `<DSH_HOME>/cordis.patch.yml` |
| Profile | `<DSH_HOME>/profiles/<profile>/cordis.patch.yml` |

The launcher owns exactly one loader entry in that file, delimited by
`# dsh-launcher providers begin` / `# dsh-launcher providers end`:

```yaml
- name: '@deepseek-ai/dsh-llm-pi-ai'
  config:
    providers:
      deepseek-official:
        apiKeyEnv: DEEPSEEK_API_KEY
        api: anthropic-messages
        baseURL: https://api.deepseek.com/anthropic
        models:
          - id: deepseek-chat
            contextWindow: 1000000
```

`config.providers` is a **dict keyed by route name**. Saving splices only the
managed entry — every other line (bundle inserts, hand-written overrides,
`!!js` scalars) is preserved byte-for-byte, so the launcher never reformats or
reorders unrelated configuration.

**Changes take effect on the next request** — DSH resolves profiles per request,
so no restart is needed.

> The legacy `settings.yaml` is not used: DSH imports it once into profile
> patches and renames it `settings.yaml.imported`. Writing provider routes there
> has no effect.

## Configuring a route

**Add Route** opens the editor. A route has:

- **Route name** — the `providers` dict key; unique within the scope,
  `[A-Za-z0-9_-]{1,64}`.
- **Display name** — optional label for selector surfaces.
- **Wire protocol** (`api`) — `anthropic-messages`, `openai-completions`, or
  `openai-responses`. Leave empty to inherit the installed catalog's protocol.
- **Base URL** — optional; when set it must start with `http://` or `https://`.
  Leave empty to inherit the catalog endpoint.
- **Model catalog** — rows of `id` / display name / context window / reasoning
  efforts. A hand-declared route (one with no `api`) needs at least a base URL
  and models; a catalog route may omit them and inherit.
- **API key env var** — the *name* of the variable holding the key, e.g.
  `DEEPSEEK_API_KEY`; the secret itself never lands in the patch file.

The **Preset** picker pre-fills the form from a built-in template
(`deepseek-official`, `openai-compatible`, `anthropic-official`,
`custom-endpoint`). **Import Preset** appends a preset's route directly to the
scope, skipping any name already present.

Unrecognised keys on a route or model (`compat`, `retryPolicy`,
`defaultContextWindow`, …) are preserved on save — the editor only owns the
fields it shows.

## Credentials

A route's key is resolved at launch from the first layer that defines it:

1. the instance's environment-variable overrides (highest),
2. `<DSH_HOME>/.credentials.yaml`,
3. the profile's `.env`,
4. `$DSH_HOME/.env` (lowest).

The editor's credential field writes to layer 2 only. The resolved value is
shown masked (`sk-***f456`) with the layer it came from. When the instance
override layer provides the value, the field is disabled: remove the override in
the instance's Environment tab first. Leaving the field empty keeps the existing
credential untouched rather than clearing it.

The route table's **Credential** column reflects whether the env var currently
resolves through any layer. It is advisory — a lookup failure shows as missing,
and never blocks the table.

## Validation

**Validate All** runs offline checks over every route in the current scope and
opens a report. It makes no network calls. Each route is reported `ok`,
`warning`, `error`, or `unknown`; the report flags blocking errors when any
route errors. Validation is advisory and does not block launching.

## Bundles

A modpack export can carry provider route templates (the **Provider templates**
option). Export reads the routes from the profile's patch layer and masks
secrets: `apiKeyEnv` is replaced with the placeholder `YOUR_API_KEY_ENV_VAR` and
unrecognised keys are dropped, so no credential leaves the machine. The template
ships at `home/provider-templates/routes.yaml`.

On import the template is merged into the target profile's `cordis.patch.yml`
append-only: a route whose name already exists is skipped, never overwritten, so
importing a pack cannot silently clobber a hand-edited route. Imported routes
carry the placeholder env var, so credentials still have to be filled in per
instance after import.

## Notes

- Reads and writes resolve through the instance's `home_id`; the shared
  (`__dedicated__`) pseudo-home is not editable.
- Route rows are sorted by name on every write, so the file diff stays stable.
- The same instance can be edited from the WebUI; the Providers tab re-reads
  from disk on refresh, so an out-of-band change is picked up on reload rather
  than merged.
