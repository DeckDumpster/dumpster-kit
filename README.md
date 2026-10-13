# dumpster-kit
Shared, game-agnostic components extracted from pokedumpster and dumpstar (tenancy, tcgcsv, ui, lakehouse)

## Gate and canary

`.github/workflows/ci.yml` runs on GitHub-hosted runners: fmt, clippy, unit tests, then a consumer
canary. `scripts/canary.sh <name> <checkout>` patches every kit crate in the consumer to this
checkout and runs its tier-1 tests (`cargo test`); its log names the consumer and kit revisions.
`tests/canary-test.sh` is its acceptance (a breaking change is red, a harmless one is green).

The kit is public and has no deploy. Consumers pin a tagged revision as a plain git dependency
and move only through their own bump bead.

Access the canary needs: repository secret `CONSUMER_READ_TOKEN`, read-only on
`DeckDumpster/pokedumpster` and `DeckDumpster/dumpstar` (both private). Until granted, the
canary job is red by design.
