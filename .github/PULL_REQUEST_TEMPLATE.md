## What does this change?

<!-- A short description, and the issue it fixes, e.g. "Fixes #12". -->

## How was it tested?

<!-- New or changed tests, and anything you checked by hand (printer model, OS). -->

## Checklist

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `npm test`, `npm run typecheck:test` and `npx tsc --noEmit` pass (in `app/`), if the frontend changed
- [ ] New user-facing text has a German translation in `app/src/i18n.ts`
- [ ] No new network access beyond printers the user connects to
- [ ] README / docs updated if behaviour changed
