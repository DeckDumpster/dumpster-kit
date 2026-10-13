Conformance corpora: data files, one directory per corpus, run by the kit and by each consumer.

## Corpora

| dir | file | rows | run against |
|-----|------|------|-------------|
| `handles/` | `cases.json` | `value`, `valid` | handle validator and the registry `handle` CHECK |
| `ulids/` | `cases.json` | `value`, `valid` | `database_id` validator and the tenant file path |
| `emails/` | `cases.json` | `value`, `valid` | `normalise_email` and the registry `email` CHECK |
| `jwt/` | `negative.json` | `name`, `describes`, `status` | Access verification: every row must be refused |

Load them with `dumpster-kit-conformance` (`handle_cases()`, `ulid_cases()`, `email_cases()`,
`negative_jwts()`); a consumer's CI runs them against whatever validators it uses.
