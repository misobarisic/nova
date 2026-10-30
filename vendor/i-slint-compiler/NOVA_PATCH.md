# Nova compiled component embedding patch

This directory vendors the published `i-slint-compiler` **1.18.0** crate from
crates.io, copied from the matching Cargo registry source. Its upstream project
is <https://github.com/slint-ui/slint>; the retained `.cargo_vcs_info.json`
identifies commit `bd20dab8529add087b5cbc81aec70bf30861ae4c` and source directory
`internal/compiler`. All upstream license files, source copyright notices,
package manifests and dependency versions are preserved. Registry installation
markers `.cargo-ok` and `.cargo-checksum.json` are omitted because this is a
modified local path dependency.

The modified upstream sources are `generator/rust.rs` (compiled root
attachment), `passes/remove_aliases.rs` (preserving consumer property storage
across compiled-library boundaries), `passes.rs` (shared source translation
catalogs), and `typeloader.rs` (Cargo source-library directory metadata). Workspace
Slint dependencies are pinned exactly to 1.18.0 so native field layouts and the embedding constructor match.

## Why this patch exists

Slint 1.18.0 exposes `ComponentFactory` and `ComponentContainer`, but its Rust
code generator cannot attach an exported compiled root to a factory's parent
slot. `new_with_existing_window` replaces the existing window root and clears
its interaction state; it is not an embedding constructor. Interpreter roots
have a separate embedding path, which this patch follows for compiled roots.

The alias pass also folds consumer properties into imported globals even
though the globals' field layouts and bindings are already compiled. A consumer
two-way binding then lowers with a global endpoint where LLR expects local
storage (`not a local reference`). The patch retains runtime two-way links for
cross-component alias pools containing imported library members. It preserves
same-component library aliases and considers the complete accumulated pool, so
indirect merges cannot cross the boundary either.

For bundled translations, `SLINT_BUNDLE_TRANSLATIONS_DOMAIN` overrides only the
catalog filename. Nova's build helper sets it to `nova-ui`, so every compiled
unit reads `crates/ui/translations/<lang>/LC_MESSAGES/nova-ui.po` directly.
Without the override, the configured translation domain still supplies the
filename. Package/library metadata and unbundled gettext domains stay unchanged.
Slint's build helper watches the source catalog directory and files; no generated
catalog is written into its own watched inputs. This avoids repeated rebuilds
caused by recreating `OUT_DIR/translations` after Cargo's build-start timestamp.

AppWindow's nonvisual `page-bindings.slint` components connect backend properties
to the shared bridge pools once for the window's lifetime. Conditional hosts
only supply mount-specific one-way inputs, and each factory product connects
fresh page properties. Do not merge already-connected pools again on remount:
Slint 1.18's `Property::link_two_way` is not idempotent and can install a binding
that points to its own common property, panicking with `Recursion detected`.

## Generated API

When the Slint compiler's experimental option is enabled, every exported
Window-rooted component additionally receives:

```rust
pub fn new_embedded(
    ctx: i_slint_core::component_factory::FactoryContext,
    prepare: impl FnOnce(&Self),
) -> Result<Self, slint::PlatformError>
```

Each component also exposes one experimental accessor for each directly imported
compiled library. Names append the library name in lowercase; imports are
uniquely identified and sorted by library name. For `NovaUiShared` this is:

```rust
pub fn shared_globals_novauishared(
    &self,
) -> Rc<nova_ui_shared::NovaUiSharedSharedGlobals>
```

Window-rooted components additionally receive a constructor that uses the
supplied imported-library bundles without duplicating their globals:

```rust
pub fn new_embedded_with_shared_globals(
    ctx: i_slint_core::component_factory::FactoryContext,
    novauishared: Rc<nova_ui_shared::NovaUiSharedSharedGlobals>,
) -> Result<Self, slint::PlatformError>
```

For multiple directly imported libraries there is one required `Rc` argument
per library, ordered by library name; for no imports the constructor takes only
`ctx`. Library Rust module configuration is honored in the argument/accessor
types. All libraries participating in this generated API must use this patched
compiler at the same pinned version.

Pass the bundle from a fully initialized owning component, for example
`app.shared_globals_novauishared()`. Uninitialized supplied bundles return an
error before allocating the component tree. Each embedded component creates a
fresh root wrapper, attachment tuple and component-local globals, while reusing
the exact supplied library `Rc` and its exported global pointers. Reused library
globals never rerun initialization; only newly allocated local/library globals
initialize. As a result, settings/models/callback handlers have one shared
identity across the main window and leaves. Property linking and callback relays
are unnecessary for these imported globals.

Adapter inheritance validates the entire imported graph before modifying it.
An already inherited pointer-identical adapter is accepted, and a different
adapter returns an error; inherited globals are never reset. Initialized bundles
also validate against their original root's adapter, including when their own
adapter cell has not yet been seeded by the ordinary host constructor. Bundles
whose original owning root has expired are rejected. The bundle retains
its original owner/root weak reference and callbacks, and its lifetime remains
owned by the main root and active leaves. Each leaf still owns its own local
root metadata and UI timers. A bundle cannot cross independent host windows.

Generated consumers must depend directly on the matching `i-slint-core` crate,
because Slint 1.18.0 does not publicly re-export `FactoryContext`. Non-windowed
system tray components do not receive this API.

The constructor rejects an expired parent, an invalid/non-dynamic parent slot,
or a parent without an existing window adapter. It does not create a fallback
window. It stores the exact dynamic slot and inherits the parent's adapter
before eager globals, item registration and component bindings initialize.
Inherited adapters also propagate into imported compiled-library globals.

The `prepare` closure runs after the root's `SharedGlobals` and inherited
adapter are installed and eager globals initialize, but **before** root/item
registration, root/subcomponent binding installation and component `user_init`.
Only exported globals may be accessed in this phase: child UI properties,
functions, layout, focus and item traversal are not initialized. Connect global
properties and callbacks here so the eventual UI bindings attach to the bridged
property pools. Separately compiled globals are separate objects; the application
must explicitly bridge every mutable shared property and callback when using the
compatibility `new_embedded` constructor with duplicated globals. Use
`Property::link_two_way(child_property, parent_property)` so the host's value and
binding win. Eager global initialization precedes `prepare`, matching the
existing low-level construction sequence; initialization-dependent global
configuration therefore needs separate consideration. Ordinary constructors
pass a no-op preparation closure to the same private helper.

The constructor does not call `WindowInner::set_component`, show/hide/run the
host window, or recursively instantiate the host tree. `ComponentContainer`
publishes the returned product, binds its root dimensions and instantiates its
children. Calling host tree traversal during `prepare` can reenter the active
factory and must be avoided.

## Runtime boundaries

- Only an exported root reads the embedding tuple in `parent_node`; popup and
  repeated trees keep their existing generated parent relationships. The tuple
  points at the dynamic slot, which core parent traversal converts to its
  container item. This includes ancestor transforms/clipping and focus traversal.
- Embedded roots use the host adapter for rendering, platform colors, metrics,
  focus and popup operations. No host root or interaction state is replaced.
- Popup globals clones retain the embedding tuple, but popup parent traversal
  does not read it; popup registration and cleanup keep upstream behavior.
- Existing pinned drop unregisters embedded items against the inherited adapter
  and frees their resources. Core cleanup closes popups whose anchor has gone
  away. The container owns the returned root; application callbacks should use
  weak host handles to avoid reference cycles.
- `ComponentHandle::window`, `show`, `hide` and `run` still refer to the shared
  host window. Use factory products as embedded handles, not standalone window
  lifecycle controllers.
- Both compilation units must bundle the same language list in the same order.
  Slint stores bundled-language selection once per context, while each generated
  compilation unit indexes its own translation tables. Nova shares catalogs and
  translation configuration between host and Settings.
- The vtable's late `embed_component` behavior remains upstream. This constructor
  performs attachment before initialization; it does not support reparenting an
  existing handle or embedding the same handle in multiple containers.

Ordinary `new`, `new_with_context` and `new_with_existing_window` keep their
existing initialization and window ownership behavior. The new root helper is
called with no embedding for those paths.

The separate Settings crate and the full workspace pass default-Nix Cargo
checks. Default-target workspace Clippy (`--locked -- -D warnings`) and
`cargo fmt --all -- --check` also pass. Runtime tests are deferred at the user's
request; checking generated Rust does not validate focus, popup, rendering or
teardown behavior. See the production measurements in
[`crates/ui/experiments/modules/README.md`](../../crates/ui/experiments/modules/README.md).

## Source-only UI libraries

Nova Kit's `nova-ui-base` exports Slint source files rather than another native
shared-globals bundle. `find_file_in_library_path` first honors explicit library
paths, then accepts Cargo's `DEP_<LIBRARY>_SLINT_SOURCE_DIR` metadata as the source
directory for `@Library/file.slint` imports. Its existing file loader and import
semantics remain unchanged. Nova's root build script forwards this directory to
rustc so inline `slint!` fixtures resolve the same files as page build scripts.
The source controls continue to import the existing compiled `NovaUiShared`
globals, preserving canonical types, global state and embedded factories.
Other consumers can use ordinary `CompilerConfiguration::with_library_paths`
with the kit's exported source directory and their own compatible host contract.
