---
description: "Finalize work: cargo fmt, clippy, test, build. Run before commits or when the user says we're done."
user_invocable: true
---

# /done

Run finalization steps in order. Stop on first failure:

1. `cargo fmt`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test`
4. `cargo build`

Report results concisely — only show errors/warnings/failures. If all four pass,
say "Clean." and nothing else.

**IMPORTANT:** fmt and clippy should NOT be run outside of /done unless the user
explicitly asks. Running fmt mid-session changes file contents and breaks Edit
tool matching. Tests can be run mid-session if needed to verify a step works.

## Optional: commit phase

If `/done` is followed by phrasing that asks for a commit (e.g.
*"with a commit for the above changes"*, *"and commit it"*, *"and commit
those changes"*), then **after** the four steps above pass cleanly, ALSO:

5. `git status` and `git diff --stat` to see what's actually changed.
6. Stage the relevant changes with `git add -p` (interactive — review each
   hunk; do NOT just `git add -A` unless every change is intentional and
   in-scope for this commit). For unambiguous cases (all changes belong to
   the same logical suite of work the user just approved), `git add` of
   specific file paths is fine and avoids the interactive prompt.
7. `git commit` with a subject and body that summarize the *suite* of
   changes done in this conversation — not just the last edit. Use a
   HEREDOC to preserve formatting.

Commit message rules:
- Subject line ≤ 70 chars, imperative mood, no trailing period.
- Blank line, then a body that explains the *why* — what problem this
  suite of changes solved, the user-visible behavior, any non-obvious
  design decisions. Reference file/function names when it helps.
- Always include the standard Co-Authored-By trailer.
- Never `--no-verify`, never `--amend` unless the user explicitly asks.
- Never `git add` files that look like secrets (`.env`, credentials, etc.)
  — warn the user instead.

If hooks fail during the commit, fix the underlying issue and create a NEW
commit (do not amend). Report the final `git log -1 --oneline` after the
commit succeeds.


