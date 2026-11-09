# Publishing heap-profiler

Releases publish wheels and the source distribution to PyPI with GitHub
Actions [Trusted Publishing](https://docs.pypi.org/trusted-publishers/): the
workflow requests a short-lived OIDC token, so no PyPI password or API token is
stored anywhere. The PyPI project is owned by the `yoavalon` account.

## One-time setup

On PyPI, signed in as `yoavalon`, open
[Publishing](https://pypi.org/manage/account/publishing/) and add a pending
GitHub publisher:

| Field | Value |
| --- | --- |
| PyPI Project Name | `heap-profiler` |
| Owner | `yoava333` |
| Repository name | `heap-profiler` |
| Workflow name | `CI.yaml` |
| Environment name | `pypi` |

The first successful upload creates the project and turns the pending publisher
into a regular one.

The GitHub `pypi` environment only accepts deployments from `v*` tags. Once the
repository is public, also add yourself as a required reviewer under
Settings → Environments → `pypi`, so every upload waits for approval. GitHub's
free plan does not allow reviewers on private repositories.

## Releasing

1. Set the new version in `Cargo.toml`, which is also the Python package
   version, and run `cargo update -p heap-profiler` to refresh `Cargo.lock`.
2. Commit, push to `main`, and wait for CI to pass.
3. Tag the commit and push the tag:

   ```sh
   git tag v0.1.9
   git push origin v0.1.9
   ```

CI builds every wheel and the sdist and runs the tests and the performance
gate. The release job then checks that the tag equals `v` plus the
`Cargo.toml` version, and publishes everything to PyPI. A mismatched tag fails
before anything is uploaded.

PyPI never accepts the same version twice, even after deleting it, so a failed
or bad release needs a new version number.
