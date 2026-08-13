# Memos Calendar And List Design QA

- Source calendar target: `/home/skynit/.codex/generated_images/019ffa1a-1077-7b00-b728-7ec59940c015/exec-d6fcb108-ca4e-4deb-a797-0347be7f9841.png`
- Source list target: `/home/skynit/.codex/generated_images/019ffa1a-1077-7b00-b728-7ec59940c015/exec-ba0204cc-6e19-4670-841c-5b7a4bc85a0a.png`
- Implementation calendar: `/home/skynit/workspace/sky/.codex/previews/memos-calendar-final-1440x900.png`
- Implementation list: `/home/skynit/workspace/sky/.codex/previews/memos-list-final-1440x900.png`
- Implementation modal: `/home/skynit/workspace/sky/.codex/previews/memos-modal-final-1440x900.png`
- Combined comparison: `/home/skynit/.codex/visualizations/2026/08/13/019ffa1a-1077-7b00-b728-7ec59940c015/memos-calendar-comparison.png`
- Viewport: `1440x900`
- State: August 2026; browser fallback reports the factual `desktop_bridge_unavailable` state.

**Full-view comparison evidence**

- Calendar renders all six equal-height weeks at `76px` each. The last week is inside the calendar surface and no horizontal overflow exists.
- Month navigation remains left aligned and the approved calendar/list segmented control remains right aligned.
- The implementation retains the application's established light art-tech theme. Layout, density, interaction hierarchy, and semantic colors follow the selected target without creating a one-page dark-theme exception.
- The browser fallback cannot render stored note rows because the Tauri bridge is unavailable. No fake note content was introduced to make the screenshot look populated.

**Focused region comparison evidence**

- Calendar cells have stable `76px` heights, room for two or three ellipsized body previews, and distinct icon plus color states for active and completed notes.
- Double-clicking `2026-08-13` opens an `aria-modal` dialog centered at `560px` width. The calendar width and position remain unchanged behind it.
- List mode switches to the table surface and removes the calendar DOM. Package tests cover descending timestamp order, left completion controls, and `active`/`completed` persistence.
- Icons use the project's `lucide-vue-next` dependency. No raster UI assets or custom SVG replacements are required.

**Findings**

- No actionable P0/P1/P2 differences remain.
- P3: the generated targets use a dark palette while the current product applies a global light art-tech theme. Preserving the product-wide theme is intentional; changing it only for Memos would be inconsistent.

**Comparison history**

1. P1: the legacy `arttech.css` Memos block overrode the new layout with `min-height: 56px` cells and clipped the last two weeks. Added current-view overrides, removed the count-dot treatment, and verified six complete equal-height rows.
2. P1: the initial editor occupied a right-side grid column and compressed the calendar. Replaced it with a centered modal; verified a `560px` dialog and unchanged background workspace geometry.
3. P2: `84px` rows pushed the final week below the `1440x900` first viewport. Tuned the stable desktop row baseline to `76px`; all six weeks now fit while retaining memo-preview capacity.

**Primary interactions tested**

- Calendar/list segmented switching.
- Single-click note document loading in component tests.
- Double-click date creation modal in browser and component tests.
- Create save, completion toggle, current-view retry, month navigation, and roving date selection in component tests.
- No horizontal overflow at `1440x900`.

**Implementation checklist**

- [x] Calendar body previews with ellipsis and semantic completion state
- [x] Full six-week layout with stable row height
- [x] All-notes list ordered by update time
- [x] Completion checkbox backed by typed `completed` status
- [x] Centered create/view modal without calendar compression
- [x] Loading, error, partial-data, empty-list, and conflict reasons
- [x] Focused package tests and browser geometry verification

final result: passed
