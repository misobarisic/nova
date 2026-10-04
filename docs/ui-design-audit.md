# UI consistency audit

This is a source-based inventory to inform a later design document for a more
unified Nova interface. It covers visual styling and the related interaction
and feedback patterns across Home, Discover, My Library, Detail, Settings,
playback, and shared controls.

The source snapshot reviewed was commit `ee687bb` on 2026-10-01. Findings below
describe code that is present; they do not claim that a visual defect was
confirmed in a rendered build. The desktop app and Android app were not run for
this audit. Items marked **Check visually** need screenshot or device review
before they are treated as user-visible defects.

Priority indicates the order for follow-up, not whether a visual difference is
wrong: **High** risks misleading or losing a user action; **Medium** is a
repeated styling or interaction inconsistency; **Check visually** needs rendered
review. Some differences are intentional and are recorded after the findings.

## Findings

### Behavior and feedback

- **High — Category edit removes the category.** The pencil control calls the
  removal callback, and the Rust handler also removes that category from every
  library entry. Its keyboard activation follows the same route. The icon
  suggests editing while the action deletes data. See
  [Settings](../crates/ui/settings.slint#L2859) and
  [category removal](../src/app/library.rs#L435).
- **High — Home can have no explanation for an empty landing page.** Its empty
  message is shown only while Continue Watching is enabled. If that setting is
  off and the other Home sections have no items, the landing content contains
  neither the message nor a content row. See
  [Home](../crates/ui/home.slint#L1850).
- **Medium — The resume prompt has no dedicated keyboard or accessibility
  treatment.** It supplies pointer-driven buttons and outside-click dismissal,
  but no local focus scope, escape/back handler, button role, or accessible
  label. Check keyboard routing while the prompt is open. See
  [AppWindow](../crates/ui/appwindow.slint#L984).
- **Medium — Settings focus borders do not consistently track keyboard mode.**
  Several controls derive their `focused` state from the navigation zone and
  index without also checking `kb_active`; similar controls elsewhere gate the
  border on keyboard mode. A border can remain after switching input modes.
- **Medium — Card hover lift differs by page.** Home disables the lift for
  touch menus and active presses; Discover and Library use `has-hover` alone.
  Verify pointer, touch, and drag behavior on Android before changing the
  shared interaction rule.
- **Medium — Muted controls can still be active.** The inactive animation
  options are dimmed when the master switch is off but remain interactive.
  Record one clear meaning for dimmed, disabled, and unavailable states.
- **Medium — Inert Detail tabs look and respond like controls.** Characters and
  Artwork use muted text but retain a pointer interaction surface despite no
  available content. The selected, unavailable, and disabled appearances need
  distinct rules.
- **Medium — Back and Escape behavior varies by page.** Discover and the player
  handle Escape; other pages mainly handle Back or Backspace. Check the intended
  close behavior for each popup and modal.

### Visual foundations

- **Partially resolved 2026-10-04 — The initial theme palette centralizes colors
  and effects.** `crates/ui/theme.slint` defines `Theme.current` for Home,
  Detail, Settings, tracking, the player and shared controls, preserving their
  existing shades and alpha values. Library and Discover page bodies remain
  deferred until their redesigns. Typography and geometry remain local.
- **Medium — Page backgrounds differ.** Settings uses `#0b0b0e`; the main
  browsing pages and Detail use `#0f0f12`. Decide whether these are surface
  levels or separate page themes.
- **Medium — Surfaces and borders use different color families.** Settings
  favors neutral surfaces such as `#121214` and `#131316`; Home, Discover,
  Library, and Detail also use blue-tinted surfaces such as `#18181c`, `#232330`,
  and `#1f1f27`. Border contrasts also vary by component.
- **Medium — Accent color has several competing roles.** `#8b5cf6`,
  `#a89af5`, and muted purple fills appear as active, primary, focus, and icon
  colors in different combinations. Define roles before selecting final values.
- **Medium — Text colors lack shared semantic roles.** Primary and secondary
  copy, hints, metadata, disabled labels, and empty-state text use different
  values across pages. Example literal pairs include Detail's `#5c5c6e` on
  `#0f0f12` (2.93:1) and Settings' `#8a8a92` on `#121214` (5.46:1); validate
  actual rendered colors and intended size before drawing accessibility
  conclusions.
- **Medium — Heading and body scales vary by screen.** Main headings range from
  18px in Settings to 26px in Library and Home, and 26–32px in Discover. Detail
  and Settings subpage headings use additional sizes from 16–24px.
- **Resolved 2026-10-02 — The UI uses bundled Roboto.** `AppWindow` now imports
  Roboto Regular/Bold and declares the default family for desktop and Android;
  packaged builds embed the fonts. Compare wrapping and system fallback glyphs
  (including emoji) in rendered builds across platforms.
- **Medium — Capitalization is inconsistent.** Examples include `DETAILS`,
  `WATCH NOW`, `New Episode`, and `Next up`. Check consistency in both English
  and Croatian translations.
- **Medium — Spacing and alignment values do not follow a documented scale.**
  Main page edges use 18px; Detail content uses 16px, and its narrow top bar
  uses 10px. Card and dialog padding add further local values.
- **Medium — Corner radius is selected locally.** Cards, input fields, buttons,
  pills, and dialogs use different radii, including on controls with similar
  roles.

### Controls, cards, and selection

- **Medium — Primary actions vary in fill, contrast, and shape.** Settings pills
  use muted fills, Detail's Watch Now control uses purple, and Home's featured
  actions use translucent black capsules.
- **Medium — Back controls have no single visual or size rule.** Home uses a
  transparent 36px button; Settings uses filled 32/44px buttons; Discover and
  Detail use filled 36/44px buttons.
- **Medium — Touch sizing is uneven.** Examples include 30px stream filters,
  28px page controls, 32px switches, and 28/32px playback speed buttons. Some
  controls opt into 44px sizing on narrow layouts, while others keep their
  compact size.
- **Medium — Responsive sizing follows width more than input type.** Some
  controls switch at 430px while the app's narrow layout switches at 700px.
  A wide Android window can still be touch-operated with compact controls.
- **Medium — Icon styles mix paths, emoji, and text glyphs.** Material paths
  appear beside movie emoji, text arrows, a pencil glyph, and play symbols
  embedded in button text. Compare optical size, weight, and translation needs.
- **Medium — Press, hover, and focus feedback vary.** Some fills animate using
  the shared `Anim` durations; others change instantly or show no explicit
  pressed state. Focus appears as borders, fills, or a separate `▶` marker.
- **Medium — Focus markers can alter layout.** `KbMark` expands from zero to
  18px when keyboard focus becomes visible, which can move adjacent controls.
- **Check visually — Poster cards share an idea but duplicate their
  implementation.** Discover, Library, Continue Watching, and Upcoming use
  similar image, footer, and placeholder structures with local sizing and
  styling.
- **Medium — Poster-card footer heights differ.** The fixed footer allowances
  are 50px, 64px, 76px, and 88px across card families. Content varies, but the
  density rules are undocumented.
- **Check visually — Detail's fixed poster proportion differs from browse
  cards.** Browse cards use a 2:3 poster; the Detail poster is 120×172px.
- **Medium — Status indicators have no documented hierarchy.** Plain accent
  labels, filled pills, white check discs, small count badges, and text metadata
  communicate different states with locally chosen styles.
- **Medium — Progress rails vary by surface.** Home cards use 5px rails;
  episode and download rows use 4px. Track color, placement, and completion
  behavior also vary.
- **Medium — Selection and focus can share the same color.** Purple fills and
  borders indicate selection in some controls; white borders or full-row fills
  indicate focus elsewhere. Establish how selected, focused, and pressed states
  combine.

### Dialogs, navigation, and states

- **Medium — Dialogs use several unrelated shells.** Category assignment,
  featured-catalog selection, resume playback, player settings, and action
  sheets differ in surface, border, backdrop, padding, and radius.
- **Check visually — Fixed dialog heights may constrain translated content.**
  Category assignment and resume playback use fixed height caps; other dialogs
  follow content or scroll. Review Croatian wrapping and short landscape
  windows.
- **Check visually — Safe-area handling differs.** Home, Discover, Library,
  Detail, and Settings apply safe insets in different layout calculations.
  Grid width calculations do not subtract the horizontal insets even when page
  padding adds them; several modal surfaces do not receive safe insets.
- **Medium — Scroll indicators differ.** Detail draws a custom 3px position
  indicator while other pages use the standard ScrollView treatment.
- **Medium — Transitions do not use the same motion rules.** Action sheets
  animate in and out; several dialogs appear immediately. Settings also keeps a
  280ms page cleanup timer while animation duration can be set to zero.
- **Medium — Empty, loading, and error states use different patterns.** Empty
  results include emoji on Discover and Library, Home can show plain text,
  Settings uses inline messages, and Detail uses muted hints. Player loading
  and errors use a centered text treatment.
- **Medium — Destructive actions lack one clear visual role.** Remove actions
  appear as regular pills or sheet rows, while confirmed device removal uses a
  destructive-tinted card and purple primary button.
- **Check visually — Accessibility labels and state exposure vary.** Search
  clear controls, featured actions, pager buttons, and some menu rows define
  accessible names and roles. Navigation icons, Detail top-bar buttons, player
  controls, switches, and other custom inputs lack equivalent explicit
  annotations. Verify the screen-reader tree before treating this as a defect.
- **Audit coverage gap — Existing UI tests assert geometry and interaction, not
  rendered appearance.** No screenshot or pixel-baseline test was found.

## Existing patterns to retain

- `SideNav` and `BottomNav` share the same four destinations and purple active
  marker; their placement is a responsive layout change.
- `SearchField`, `Dropdown`, `SpeedControl`, `icons.slint`, and `Anim` already
  provide shared pieces to build on.
- The featured Home showcase uses a pale-yellow title on narrow layouts, and
  player controls use high-contrast surfaces over video. Keep these as deliberate
  context-specific treatments unless a later direction replaces them.
- Desktop context menus and touch action sheets provide platform-specific
  interaction patterns. Review their labels, states, and visual roles together.

## Visual review checklist

Review screenshots or rendered builds at 320px, 360px, 430px, 699px, 700px,
and desktop widths; include Android landscape with cutouts. Check English and
Croatian, keyboard and pointer input, touch press and drag, and animations on
and off. Cover populated, empty, loading, error, selected, focused, disabled,
and open-dialog states. Record actual screenshots separately from source-based
findings, and update the priority after visual review.

This audit does not prescribe a final palette, type scale, component API, or
implementation sequence. Those decisions belong in the follow-up design doc.
