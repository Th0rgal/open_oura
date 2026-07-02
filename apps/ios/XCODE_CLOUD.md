# Xcode Cloud → auto TestFlight on merge (model-free)

A clean Xcode Cloud checkout has **none** of the gitignored build inputs
(`OuraCore.xcframework`, `OuraApp.xcodeproj`, libtorch, the `.ptl` models, `oura.db`).
So CI builds the **model-free** target: it rebuilds the Rust xcframework and generates
the project from `project-ci.yml` in `ci_scripts/ci_post_clone.sh`. The app ships the
model-free summary and **syncs from a real ring over BLE**; the on-device
hypnogram / CVA / activity models are not in CI (they need libtorch + `.ptl`).

## One-time setup (your Apple account)

1. In **App Store Connect → your app → Xcode Cloud** (or Xcode → Product → Xcode Cloud),
   create a workflow.
2. Source: this repo, **start condition = push to `main`** (or the PR branch).
3. Environment: latest Xcode. Xcode Cloud auto-runs `ci_scripts/ci_post_clone.sh`.
4. Action: **Archive** the `OuraApp` scheme → **Post-action: TestFlight (Internal)**.
5. Signing is automatic (Xcode Cloud manages it); the bundle id is `md.thomas.openoura`.

That's it — each merge to `main` produces a TestFlight build.

## Adding the on-device models to CI later

The torch models are deliberately out of CI because:
- **libtorch** (~80 MB of dylibs) is too slow to build per run → must be **vendored**
  (build once with `spike/build_libtorch_ios.sh device` + `package-libtorch-xcframeworks.sh`,
  then commit via **Git LFS** or fetch a release asset in `ci_post_clone.sh`).
- the **`.ptl` models are decrypted/sensitive** and must NOT go in git — host them in a
  private store and fetch with a pre-signed URL kept in an Xcode Cloud **secret** env var.

Once both are fetched in `ci_post_clone.sh`, point the workflow at `project.yml` (the
full torch spec) instead of `project-ci.yml`.
