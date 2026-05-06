# Contributing to odrive-linux

Thanks for the interest. A few ground rules.

## License

This project is licensed under the **GNU Affero General Public License v3.0 or later** (AGPL-3.0-or-later). See [LICENSE](LICENSE) for the full text. By contributing, you agree that your contributions are licensed under the same terms.

## Developer Certificate of Origin (DCO)

Every commit must be **signed off** to certify that you wrote it (or otherwise have the right to submit it under the project's license). We use the standard [Developer Certificate of Origin v1.1](https://developercertificate.org/) — the same lightweight mechanism the Linux kernel and many other projects use. There is no separate CLA to sign.

What you're certifying when you sign off:

> By making a contribution to this project, I certify that:
>
> (a) The contribution was created in whole or in part by me and I have the right to submit it under the open source license indicated in the file; or
>
> (b) The contribution is based upon previous work that, to the best of my knowledge, is covered under an appropriate open source license and I have the right under that license to submit that work with modifications, whether created in whole or in part by me, under the same open source license (unless I am permitted to submit under a different license), as indicated in the file; or
>
> (c) The contribution was provided directly to me by some other person who certified (a), (b) or (c) and I have not modified it.
>
> (d) I understand and agree that this project and the contribution are public and that a record of the contribution (including all personal information I submit with it, including my sign-off) is maintained indefinitely and may be redistributed consistent with this project or the open source license(s) involved.

### How to sign off

Pass `-s` to `git commit`:

```bash
git commit -s -m "your message"
```

That appends a line like this to the commit message:

```
Signed-off-by: Your Name <your.email@example.com>
```

Use your real name and a real email address. Anonymous and pseudonymous sign-offs are not accepted.

To set this up once and forget about it, you can alias it:

```bash
git config --global alias.cs 'commit -s'
```

If you forget to sign off, amend the most recent commit with `git commit --amend -s --no-edit`, or for a series use `git rebase --signoff main`.

PRs without sign-offs on every commit will be asked to rebase before merge.

## Style and scope

- Run `cargo fmt` and `cargo clippy --workspace` before pushing.
- Keep PRs focused. One feature or bug fix per PR; refactors as separate PRs.
- Add tests for new behaviour in `odrive-core` where it's practical (the existing 68 unit tests are a good template — config round-trip, parser, threshold encoding).
- The GUI and Nautilus / Dolphin integrations are harder to unit test; manual reproduction steps in the PR description are fine for those.

## Reporting issues

Open a GitHub issue with:
- What you tried,
- What you expected,
- What actually happened,
- The relevant slice of `~/.odrive-agent/log/main.log` if the agent was involved.
