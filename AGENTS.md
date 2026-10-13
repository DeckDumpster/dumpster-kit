# dumpster-kit — agent brief

Shared, game-agnostic components for pokedumpster and dumpstar (tenancy, tcgcsv, ui, lakehouse).

## Before `work submit`

Run the tiers your change touches, on your committed branch:

    testenv --suites <tiers touched> <branch> dumpster-kit

Tiers are declared in `.spira/testenv.toml` (currently `rust`: fmt, clippy, test). Add a tier
there whenever you add a CI job, with the same argv the job runs.
