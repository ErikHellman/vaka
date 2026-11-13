# Releasing the Python SDK (`vaka-sdk` on PyPI)

Releases are built and published by [`.github/workflows/release-python.yml`](../../.github/workflows/release-python.yml)
with [PyPI trusted publishing](https://docs.pypi.org/trusted-publishers/): GitHub Actions proves
its identity to PyPI with OIDC, so no API token is stored anywhere.

## One-time setup

1. **PyPI account.** Sign in at <https://pypi.org> (enable 2FA, PyPI requires it). Do the same on
   <https://test.pypi.org> if you want dry runs; it is a separate account.
2. **Pending trusted publisher.** The project doesn't exist on PyPI yet, so register a *pending*
   publisher, which creates the project on the first upload. Go to
   <https://pypi.org/manage/account/publishing/> and fill in:

   | Field | Value |
   | --- | --- |
   | PyPI project name | `vaka-sdk` |
   | Owner | `ErikHellman` |
   | Repository name | `vaka` |
   | Workflow name | `release-python.yml` |
   | Environment name | `pypi` |

   Repeat on <https://test.pypi.org/manage/account/publishing/> with environment `testpypi`.
   After the first release, the publisher shows under the project's *Settings → Publishing*.
3. **GitHub environments.** In the repository, *Settings → Environments*, create `pypi` and
   `testpypi`. For `pypi`, add yourself as a required reviewer so every upload waits for a click,
   and limit deployment to tags matching `python-v*` (plus `main` if you will publish from a manual run).

## Release steps

1. Bump `__version__` in [`src/vaka/_version.py`](src/vaka/_version.py) (PEP 440: `0.3.0`,
   `0.3.0rc1`). PyPI never accepts the same version twice, even after deleting a file.
2. Update the docs if the API changed (`docs/sdk-python.md`, `docs/install/python.md`), commit, and
   merge to `main`.
3. Optional dry run: *Actions → Release Python SDK → Run workflow* with `repository: testpypi`, then
   `pip install -i https://test.pypi.org/simple/ vaka-sdk==<version>` in a clean venv.
4. Release: push a tag `python-v<version>` on the release commit, for example

   ```sh
   git tag -a python-v0.3.0 -m "vaka-sdk 0.3.0" && git push origin python-v0.3.0
   ```

   The workflow checks that the tag matches `_version.py`, runs the tests, builds the sdist and
   wheel, runs `twine check`, and publishes to PyPI after the `pypi` environment is approved.
   (A manual run with `repository: pypi` publishes the dispatched commit instead; use it when you
   cannot push tags, such as from a cloud session.)
5. Check <https://pypi.org/project/vaka-sdk/> and install it in a clean venv:
   `pip install "vaka-sdk[otel]==0.3.0" && python -c "import vaka; print(vaka.__version__)"`.

## Building locally

```sh
cd sdks/python
python -m pip install build twine
python -m build            # dist/vaka_sdk-<version>.tar.gz and -py3-none-any.whl
twine check --strict dist/*
```

Manual upload (only if CI is unavailable): create an API token scoped to `vaka-sdk` at
<https://pypi.org/manage/account/token/> and run `twine upload dist/*` with username `__token__`.

## Yanking

If a release is broken, *yank* it on PyPI (project → *Manage* → release → *Options → Yank*) and
publish a fixed version. Yanked versions stay installable when pinned exactly, but resolvers skip them.
