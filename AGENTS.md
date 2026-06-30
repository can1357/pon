# Dependency Management Policy & Workspace Contracts

All agents (and human developers) operating on the `pon` workspace must strictly adhere to the centralized dependency management pattern. This policy prevents dependency fracturing, ensures unified versioning across all modules, and simplifies workspace builds.

---

## 1. Centralized Dependency Declaration

### The Golden Rule
*   **All dependencies—both external and internal—must be declared in the root `Cargo.toml` file under the `[workspace.dependencies]` table.**
*   **Individual member manifests (e.g., `pon-codegen/Cargo.toml`) must never specify inline versions, git repositories, or features directly.** They must always inherit from the workspace.

### How to Add/Use a Dependency

#### Step 1: Declare in the Root `Cargo.toml`
Add the dependency under the `[workspace.dependencies]` table in the root manifest.

```toml
# Root Cargo.toml
[workspace.dependencies]
# External dependencies
anyhow = "1"
cranelift-codegen = "=0.133.1"

# Internal path dependencies
pon-gc = { path = "pon-gc" }
```

#### Step 2: Inherit in Member `Cargo.toml`
In the member crate's manifest, import the dependency by setting `workspace = true`.

```toml
# Member Cargo.toml (e.g., pon-runtime/Cargo.toml)
[dependencies]
anyhow.workspace = true
pon-gc.workspace = true
```

---

## 2. Prohibited Practices

*   **No Inline Versions:** Never write `anyhow = "1.0.80"` in a member crate. It must be `anyhow.workspace = true`.
*   **No Direct Git Dependencies:** Git source declarations must live only in the root `Cargo.toml`'s dependency registry.
*   **No Feature Drift:** Shared features should be declared and managed in the root dependency configuration where possible.

---

## 3. Workspace Lint Inheritance

To ensure warning levels and Clippy checks are consistently enforced across all crates, every member manifest must inherit the workspace-wide lints definition.

Every member crate's `Cargo.toml` **must** contain:

```toml
[lints]
workspace = true
```

If you create a new crate or refactor an existing manifest, ensure this block is always present immediately after the package settings.
