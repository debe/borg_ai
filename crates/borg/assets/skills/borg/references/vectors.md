# Vectors

Word matching (BM25) picks the lessons the turn-end check reads. It misses lessons that share no rare
word with the work: paraphrases, German work against English lessons, renamed identifiers. Vectors add
up to `BORG_VECTOR_K` (10) such lessons to that shortlist; the model still decides what applies. Only
the turn-end check uses them; prompts, early delivery and the free gate stay word-based.

Vectors are on by default: built into the default `borg` binary and enabled unless `BORG_VECTORS=off`.
They need a model file fetched once and an index per collective. `borg setup` does both unless it is
given `--no-vectors`.

## Commands

```sh
borg vectors                    # model and index status for the collective the current directory routes to
borg vectors --fetch            # download the model once (~490 MB, pinned revision, sha256 checked)
borg vectors --rebuild          # (re)build the index of the collective the current directory routes to
borg vectors --rebuild --store ~/.local/share/borg/collectives/<name>/lessons.jsonl   # any collective
```

The index (`<store>/vectors/`) updates itself after `borg learn`, `borg assimilate --apply` and a mesh
sync that brings new lessons; a missing index is built by the first of these. To have it at once after
fetching the model for a store that already has lessons, or when `borg vectors` says the index was built
with another model, run `--rebuild` once per collective. `borg doctor` names each collective that needs
it, with the exact command.

## Where things live

| What | Default | Override |
|---|---|---|
| Model (Model2Vec potion-multilingual-128M) | `~/.local/share/borg/models/potion-multilingual-128M` | `BORG_VECTOR_MODEL` (a directory) |
| Index | `<store>/vectors/` beside `lessons.jsonl` | the store (`BORG_HOME`) |
| Download source | Hugging Face, `minishlab/potion-multilingual-128M` at a pinned revision | `BORG_VECTOR_URL` |

## Mirrors, proxies, offline machines

`--fetch` runs `curl` for `<base>/config.json`, `<base>/tokenizer.json` and
`<base>/model.safetensors`, where `<base>` is `BORG_VECTOR_URL` or the Hugging Face URL of the pinned
revision. Each file must match its pinned sha256, so any mirror must serve exactly those files.

- **Company mirror:** copy the three files from the pinned revision onto an internal HTTPS server or
  artifact store, then `BORG_VECTOR_URL=https://mirror.example/potion-multilingual-128M borg vectors --fetch`.
- **No network:** curl reads `file://` URLs: `BORG_VECTOR_URL=file:///mnt/share/potion borg vectors --fetch`.
  Copying a colleague's whole model directory to the default path (or `BORG_VECTOR_MODEL`) works too.
- **HTTP proxy:** curl honours `HTTPS_PROXY`, `https_proxy` and `NO_PROXY`. A proxy that re-signs TLS
  needs its CA: `CURL_CA_BUNDLE=/path/to/company-ca.pem borg vectors --fetch`.

A failed fetch leaves any earlier good file in place (downloads go to `<file>.part` first) and changes
nothing else; re-run it.

## Failing open

No model, no index, a locked index or any error means word matching only: a turn end never fails
because of vectors, and no hook ever downloads anything. `BORG_VECTORS=off` turns them off entirely
(for example on a machine without disk space for the model). A binary built with
`--no-default-features` has no vectors; `borg vectors` says so.
