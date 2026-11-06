# Needs validation

No `needs_validation` records.

The carried submodule-filter confirmation was not re-executed because this host cannot enforce the required OS sandbox (no bubblewrap, `unshare --net` and `unshare --mount` fail, `CapEff` is empty, and the target tree is writable). `src/git.rs` and `SECURITY.md` are unchanged since `97ba479f6ecd577ca5acaad2f32983c852355f9f`, and the status and diff argv still omit `--ignore-submodules`. That limit did not open a new lead.

Prior audit `security-audit/2026-10-20` also had no `needs_validation` records. None were carried.
