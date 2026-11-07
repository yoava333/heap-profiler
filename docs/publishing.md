# Publishing heap-profiler

Tagged releases publish wheels and the source distribution directly to PyPI
using GitHub Actions Trusted Publishing. The release job downloads all
`wheels-*` artifacts into `dist/` and publishes that directory after builds,
tests, and the performance check pass.

Configure a pending publisher in the package owner's PyPI account before the
first upload:

- Project: `heap-profiler`
- GitHub owner: `yoava333`
- Repository: `heap-profiler`
- Workflow: `CI.yaml`
- Environment: `pypi`

The GitHub `pypi` environment can carry any desired release approval rules.
The workflow requests an OIDC identity token and uses no stored publishing
password. Creating a version tag after the pending publisher is configured
lets the first successful upload create the PyPI project.

For a manual first upload with an API token instead, build wheels/source
distribution, then use `uv publish` with credentials configured locally.
Never commit credentials to the repository.
