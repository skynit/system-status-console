<script setup lang="ts">
import { computed, nextTick, onActivated, onBeforeUnmount, onDeactivated, onMounted, reactive, ref } from 'vue'
import {
  ArrowLeft,
  Bot,
  BookOpenCheck,
  CalendarDays,
  Check,
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  CircleDot,
  Clock3,
  Coins,
  FileText,
  Folder,
  List,
  ListTree,
  LoaderCircle,
  MessagesSquare,
  RefreshCw,
  Save,
  Sparkles,
  TerminalSquare,
  Trash2,
} from 'lucide-vue-next'

import {
  captureJournalKnowledge,
  collectJournalUsage,
  deleteNote,
  fetchJournalSummary,
  getBackendHealth,
  getNote,
  listNotes,
  writeNote,
} from '../backend'
import JournalMarkdownEditor from '../components/JournalMarkdownEditor.vue'
import JournalOutlineTree, { type JournalOutlineItem } from '../components/JournalOutlineTree.vue'
import type {
  BridgeError,
  JournalCollection,
  JournalFetchInput,
  JournalKnowledgeCandidate,
  JournalSummary,
  NoteDocument,
  NoteQuery,
  NoteStatus,
  NoteSummary,
} from '../types'

type ViewMode = 'calendar' | 'list'
type WorkspaceMode = 'browse' | 'editor'
type SaveState = 'idle' | 'unsaved' | 'saving' | 'saved' | 'failed'
type FetchState = 'idle' | 'loading' | 'ready' | 'error'
type CaptureState = 'idle' | 'loading' | 'stored' | 'error'
type JournalFetchTask = {
  state: FetchState
  phase: 'collecting' | 'summarizing' | null
  error: BridgeError | null
  collection: JournalCollection | null
  summary: JournalSummary | null
  captureStates: Record<string, CaptureState>
  captureMessages: Record<string, string>
}
type CalendarDay = {
  date: string
  dayNumber: number
  inCurrentMonth: boolean
  isToday: boolean
  notes: NoteSummary[]
}

const QUERY_LIMIT = 64
const CALENDAR_CELL_COUNT = 42
const AUTOSAVE_DELAY_MS = 900
const weekdays = ['周一', '周二', '周三', '周四', '周五', '周六', '周日']

const today = startOfDay(new Date())
const todayKey = localDateKey(today)
const viewMode = ref<ViewMode>('calendar')
const workspaceMode = ref<WorkspaceMode>('browse')
const visibleMonth = ref(startOfMonth(today))
const selectedDate = ref(todayKey)
const notes = ref<NoteSummary[]>([])
const documents = ref<Record<string, NoteDocument>>({})
const loading = ref(true)
const refreshing = ref(false)
const pageError = ref<BridgeError | null>(null)
const partialReason = ref<string | null>(null)
const operationError = ref<string | null>(null)
const activeDocument = ref<NoteDocument | null>(null)
const editorLoading = ref(false)
const editorError = ref<string | null>(null)
const draftTitle = ref('')
const draftBody = ref('')
const dirty = ref(false)
const saveState = ref<SaveState>('idle')
const saving = ref(false)
const fetchTasks = reactive(new Map<string, JournalFetchTask>())
const activeFetchTask = computed(() => fetchTasks.get(selectedDate.value) ?? null)
const fetchState = computed(() => activeFetchTask.value?.state ?? 'idle')
const fetchError = computed(() => activeFetchTask.value?.error ?? null)
const collectedJournal = computed(() => activeFetchTask.value?.collection ?? null)
const fetchedSummary = computed(() => activeFetchTask.value?.summary ?? null)
const fetchPhase = computed(() => activeFetchTask.value?.phase ?? null)
const captureStates = computed(() => activeFetchTask.value?.captureStates ?? {})
const captureMessages = computed(() => activeFetchTask.value?.captureMessages ?? {})
const runningFetchDate = computed(() => (
  [...fetchTasks.entries()].find(([, task]) => task.state === 'loading')?.[0] ?? null
))
const calendarGrid = ref<HTMLElement | null>(null)
const markdownEditor = ref<InstanceType<typeof JournalMarkdownEditor> | null>(null)
let active = true
let loadGeneration = 0
let documentGeneration = 0
let editorGeneration = 0
let autosaveTimer: number | null = null

const monthLabel = computed(() => new Intl.DateTimeFormat('zh-CN', {
  year: 'numeric',
  month: 'long',
}).format(visibleMonth.value))

const notesByDate = computed(() => {
  const grouped = new Map<string, NoteSummary[]>()
  for (const note of notes.value) {
    if (!note.diaryDate) continue
    const dateNotes = grouped.get(note.diaryDate) ?? []
    dateNotes.push(note)
    dateNotes.sort((left, right) => right.updatedAtMs - left.updatedAtMs || left.id.localeCompare(right.id))
    grouped.set(note.diaryDate, dateNotes)
  }
  return grouped
})

const sortedNotes = computed(() => [...notes.value].sort((left, right) => (
  right.updatedAtMs - left.updatedAtMs || left.id.localeCompare(right.id)
)))

const calendarDays = computed<CalendarDay[]>(() => {
  const monthStart = visibleMonth.value
  const gridStart = addDays(monthStart, -mondayIndex(monthStart))
  return Array.from({ length: CALENDAR_CELL_COUNT }, (_, index) => {
    const value = addDays(gridStart, index)
    const date = localDateKey(value)
    return {
      date,
      dayNumber: value.getDate(),
      inCurrentMonth: value.getMonth() === monthStart.getMonth(),
      isToday: date === todayKey,
      notes: notesByDate.value.get(date) ?? [],
    }
  })
})

const calendarWeeks = computed(() => Array.from({ length: 6 }, (_, index) => (
  calendarDays.value.slice(index * 7, index * 7 + 7)
)))

const saveLabel = computed(() => {
  if (saveState.value === 'saving') return '正在保存'
  if (saveState.value === 'saved') return '已保存'
  if (saveState.value === 'unsaved') return '待保存'
  if (saveState.value === 'failed') return '保存失败'
  return activeDocument.value ? '已载入' : '新日志'
})

const recommendedCandidates = computed(() => (
  fetchedSummary.value?.knowledgeCandidates.filter((candidate) => candidate.recommended) ?? []
))

const knowledgeEligibilityRows = computed(() => {
  const summary = fetchedSummary.value
  if (!summary) return []

  return summary.knowledgeItems.map((item) => {
    const sourceSessionIds = new Set(item.sourceSessionIds)
    const candidates = summary.knowledgeCandidates.filter((candidate) => (
      sourceSessionIds.has(candidate.sourceSessionId)
    ))
    const recommendedCandidate = candidates.find((candidate) => candidate.recommended) ?? null
    if (recommendedCandidate) {
      return {
        ...item,
        eligible: true,
        candidate: recommendedCandidate,
        reason: '来源会话符合条件，可确认入库。',
      }
    }

    const sessions = collectedJournal.value?.sessions.filter((session) => (
      sourceSessionIds.has(session.sessionId)
    )) ?? []
    const knownSessionIds = new Set(sessions.map((session) => session.sessionId))
    const allSourcesKnown = item.sourceSessionIds.every((sessionId) => knownSessionIds.has(sessionId))
    if (allSourcesKnown && sessions.every((session) => session.eligibility.lengthClass !== 'long')) {
      return {
        ...item,
        eligible: false,
        candidate: null,
        reason: '来源会话未达到长会话门槛（至少 24 条有效消息或 12,000 字符）。',
      }
    }

    const candidateReasons = [...new Set(candidates
      .filter((candidate) => !candidate.recommended)
      .map((candidate) => candidate.reason.trim())
      .filter(Boolean))]
    if (candidateReasons.length) {
      return {
        ...item,
        eligible: false,
        candidate: null,
        reason: `总结器未推荐入库：${candidateReasons.join('；')}`,
      }
    }

    if (sessions.some((session) => session.eligibility.lengthClass === 'long')) {
      return {
        ...item,
        eligible: false,
        candidate: null,
        reason: '来源会话已达到长度门槛，但总结器未判定其主要内容适合知识入库。',
      }
    }

    return {
      ...item,
      eligible: false,
      candidate: null,
      reason: '缺少来源会话的入库资格信息，暂不可入库。',
    }
  })
})

const matchedRecommendedSessionIds = computed(() => new Set(knowledgeEligibilityRows.value
  .map((item) => item.candidate?.sourceSessionId)
  .filter((sessionId): sessionId is string => Boolean(sessionId))))

const unmatchedRecommendedCandidates = computed(() => recommendedCandidates.value.filter((candidate) => (
  !matchedRecommendedSessionIds.value.has(candidate.sourceSessionId)
)))

const knowledgeCandidateCount = computed(() => (
  knowledgeEligibilityRows.value.filter((item) => item.eligible).length
  + unmatchedRecommendedCandidates.value.length
))

const sourceGroups = computed(() => ['codex', 'claude', 'opencode'].map((source) => ({
  source,
  label: sourceLabel(source),
  coverage: collectedJournal.value?.sourceCoverage.find((item) => item.source === source) ?? null,
  usage: collectedJournal.value?.tokenUsage.bySource.find((item) => item.source === source) ?? null,
  sessions: collectedJournal.value?.sessions.filter((session) => (
    session.source === source && session.eligibility.state === 'included'
  )) ?? [],
})))

const includedSessionCount = computed(() => collectedJournal.value?.sessions.filter(
  (session) => session.eligibility.state === 'included',
).length ?? 0)

const journalOutline = computed<JournalOutlineItem[]>(() => {
  const roots: JournalOutlineItem[] = []
  const stack: JournalOutlineItem[] = []
  let headingIndex = 0

  draftBody.value.split('\n').forEach((line, index) => {
    const match = /^(#{1,3})\s+(.+?)\s*$/.exec(line)
    if (!match) return
    const item: JournalOutlineItem = {
      level: match[1].length,
      label: match[2],
      line: index + 1,
      headingIndex,
      children: [],
    }
    headingIndex += 1
    while (stack.length && stack[stack.length - 1].level >= item.level) stack.pop()
    const parent = stack[stack.length - 1]
    if (parent) parent.children.push(item)
    else roots.push(item)
    stack.push(item)
  })

  return roots
})

function focusOutlineHeading(headingIndex: number): void {
  markdownEditor.value?.focusHeading(headingIndex)
}

function startOfDay(value: Date): Date {
  return new Date(value.getFullYear(), value.getMonth(), value.getDate())
}

function startOfMonth(value: Date): Date {
  return new Date(value.getFullYear(), value.getMonth(), 1)
}

function addDays(value: Date, amount: number): Date {
  return new Date(value.getFullYear(), value.getMonth(), value.getDate() + amount)
}

function addMonths(value: Date, amount: number): Date {
  return new Date(value.getFullYear(), value.getMonth() + amount, 1)
}

function mondayIndex(value: Date): number {
  return (value.getDay() + 6) % 7
}

function localDateKey(value: Date): string {
  const year = value.getFullYear()
  const month = String(value.getMonth() + 1).padStart(2, '0')
  const day = String(value.getDate()).padStart(2, '0')
  return `${year}-${month}-${day}`
}

function dateFromKey(value: string): Date {
  const [year, month, day] = value.split('-').map(Number)
  return new Date(year, month - 1, day)
}

function currentQuery(offset: number): NoteQuery {
  if (viewMode.value === 'list') {
    return {
      search: null,
      diaryDateFrom: null,
      diaryDateTo: null,
      tags: [],
      status: null,
      deleted: 'exclude',
      sort: 'updated_desc',
      limit: QUERY_LIMIT,
      offset,
    }
  }
  const start = visibleMonth.value
  const end = new Date(start.getFullYear(), start.getMonth() + 1, 0)
  return {
    search: null,
    diaryDateFrom: localDateKey(start),
    diaryDateTo: localDateKey(end),
    tags: [],
    status: null,
    deleted: 'exclude',
    sort: 'diary_date_desc',
    limit: QUERY_LIMIT,
    offset,
  }
}

function fetchInput(): JournalFetchInput {
  const start = startOfDay(dateFromKey(selectedDate.value))
  const end = addDays(start, 1)
  return {
    localDate: selectedDate.value,
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'Asia/Shanghai',
    windowStartMs: start.getTime(),
    windowEndMs: end.getTime(),
  }
}

async function hydrateDocuments(summaries: NoteSummary[], generation: number): Promise<void> {
  const nextDocuments: Record<string, NoteDocument> = {}
  let failedReason: string | null = null
  let nextIndex = 0
  const workers = Array.from({ length: Math.min(6, summaries.length) }, async () => {
    while (nextIndex < summaries.length) {
      const summary = summaries[nextIndex]
      nextIndex += 1
      const result = await getNote(summary.id)
      if (result.kind === 'document') nextDocuments[summary.id] = result.document
      else if (failedReason === null) failedReason = result.error.reason
    }
  })
  await Promise.all(workers)
  if (!active || generation !== documentGeneration) return
  documents.value = nextDocuments
  partialReason.value = failedReason
}

async function loadNotes(): Promise<void> {
  const generation = ++loadGeneration
  const hydrationGeneration = ++documentGeneration
  refreshing.value = true
  loading.value = true
  pageError.value = null
  partialReason.value = null
  operationError.value = null

  const healthPromise = getBackendHealth()
  const collected: NoteSummary[] = []
  let offset = 0
  let error: BridgeError | null = null
  while (active && generation === loadGeneration) {
    const result = await listNotes(currentQuery(offset))
    if (result.kind === 'error') {
      error = result.error
      break
    }
    collected.push(...result.page.notes)
    if (!result.page.hasMore || result.page.nextOffset === null) break
    offset = result.page.nextOffset
  }

  await healthPromise
  if (!active || generation !== loadGeneration) return
  notes.value = error ? [] : collected
  documents.value = {}
  pageError.value = error
  refreshing.value = false
  if (!error && collected.length > 0) await hydrateDocuments(collected, hydrationGeneration)
  if (!active || generation !== loadGeneration) return
  loading.value = false
}

function setViewMode(mode: ViewMode): void {
  if (viewMode.value === mode) return
  viewMode.value = mode
  void loadNotes()
}

function changeMonth(amount: number): void {
  const nextMonth = addMonths(visibleMonth.value, amount)
  visibleMonth.value = nextMonth
  selectedDate.value = nextMonth.getFullYear() === today.getFullYear()
    && nextMonth.getMonth() === today.getMonth()
    ? todayKey
    : localDateKey(nextMonth)
  void loadNotes()
  void focusSelectedDate()
}

function goToToday(): void {
  visibleMonth.value = startOfMonth(today)
  selectedDate.value = todayKey
  void loadNotes()
  void focusSelectedDate()
}

function selectDate(date: string): void {
  selectedDate.value = date
}

function memoPreview(note: NoteSummary): string {
  const body = documents.value[note.id]?.bodyMarkdown.trim()
  return body || note.title || '无正文'
}

function statusLabel(status: NoteStatus): string {
  if (status === 'completed') return '已完成'
  if (status === 'archived') return '已归档'
  if (status === 'draft') return '草稿'
  return '记录中'
}

function isCompleted(note: NoteSummary): boolean {
  return note.status === 'completed'
}

function formatTimestamp(timestamp: number | null): string {
  if (timestamp === null) return '未知'
  return new Intl.DateTimeFormat('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(timestamp).replaceAll('/', '-')
}

function formatCompactNumber(value: number | null): string {
  if (value === null) return '未知'
  return new Intl.NumberFormat('zh-CN', { notation: 'compact', maximumFractionDigits: 1 }).format(value)
}

function sourceLabel(source: string): string {
  if (source === 'codex') return 'Codex'
  if (source === 'claude') return 'Claude'
  if (source === 'opencode') return 'OpenCode'
  return source
}

function setJournalFocus(enabled: boolean): void {
  document.documentElement.classList.toggle('journal-focus-mode', enabled)
}

async function openDay(date: string): Promise<void> {
  const generation = ++editorGeneration
  selectedDate.value = date
  workspaceMode.value = 'editor'
  setJournalFocus(true)
  editorLoading.value = true
  editorError.value = null
  activeDocument.value = null
  draftTitle.value = `${date} 工作日志`
  draftBody.value = ''
  dirty.value = false
  saveState.value = 'idle'
  const note = notesByDate.value.get(date)?.[0]
  if (note) {
    const result = await getNote(note.id)
    if (!active || generation !== editorGeneration) return
    if (result.kind === 'document') {
      activeDocument.value = result.document
      documents.value = { ...documents.value, [note.id]: result.document }
      draftTitle.value = result.document.summary.title
      draftBody.value = result.document.bodyMarkdown
      saveState.value = 'saved'
    } else {
      editorError.value = result.error.reason
      saveState.value = 'failed'
    }
  }
  if (!active || generation !== editorGeneration) return
  editorLoading.value = false
}

async function openNote(note: NoteSummary): Promise<void> {
  selectedDate.value = note.diaryDate ?? selectedDate.value
  const generation = ++editorGeneration
  workspaceMode.value = 'editor'
  setJournalFocus(true)
  editorLoading.value = true
  editorError.value = null
  const result = await getNote(note.id)
  if (!active || generation !== editorGeneration) return
  if (result.kind === 'document') {
    activeDocument.value = result.document
    draftTitle.value = result.document.summary.title
    draftBody.value = result.document.bodyMarkdown
    dirty.value = false
    saveState.value = 'saved'
  } else {
    activeDocument.value = null
    draftTitle.value = `${selectedDate.value} 工作日志`
    draftBody.value = ''
    editorError.value = result.error.reason
    saveState.value = 'failed'
  }
  editorLoading.value = false
}

function scheduleAutosave(): void {
  if (autosaveTimer !== null) window.clearTimeout(autosaveTimer)
  autosaveTimer = window.setTimeout(() => {
    autosaveTimer = null
    void saveDraft()
  }, AUTOSAVE_DELAY_MS)
}

function markDirty(): void {
  dirty.value = true
  saveState.value = 'unsaved'
  editorError.value = null
  scheduleAutosave()
}

function updateBody(value: string): void {
  if (draftBody.value === value) return
  draftBody.value = value
  markDirty()
}

async function saveDraft(): Promise<void> {
  if (saving.value || !dirty.value) return
  if (!draftTitle.value.trim() && !draftBody.value.trim()) return
  const title = draftTitle.value.trim() || `${selectedDate.value} 工作日志`
  const body = draftBody.value
  const current = activeDocument.value
  saving.value = true
  saveState.value = 'saving'
  const result = await writeNote(current ? {
    kind: 'save',
    id: current.summary.id,
    expectedRevision: current.summary.revision,
    autosave: true,
    meta: {
      title,
      diaryDate: selectedDate.value,
      tags: current.summary.tags,
      status: current.summary.status,
      pinned: current.summary.pinned,
    },
    bodyMarkdown: body,
  } : {
    kind: 'create',
    meta: {
      title,
      diaryDate: selectedDate.value,
      tags: [],
      status: 'active',
      pinned: false,
    },
    bodyMarkdown: body,
  })
  saving.value = false
  if (result.kind === 'error') {
    editorError.value = result.error.reason
    saveState.value = 'failed'
    return
  }
  if (result.result.kind === 'conflict') {
    editorError.value = 'note_revision_conflict'
    saveState.value = 'failed'
    return
  }
  if (result.result.kind !== 'stored') {
    editorError.value = 'journal_save_not_stored'
    saveState.value = 'failed'
    return
  }
  const stored = result.result.note
  activeDocument.value = { summary: stored, bodyMarkdown: body }
  documents.value = { ...documents.value, [stored.id]: activeDocument.value }
  if (draftTitle.value.trim() === title && draftBody.value === body) {
    dirty.value = false
    saveState.value = 'saved'
  } else {
    saveState.value = 'unsaved'
    scheduleAutosave()
  }
}

async function closeEditor(): Promise<void> {
  if (autosaveTimer !== null) {
    window.clearTimeout(autosaveTimer)
    autosaveTimer = null
  }
  await saveDraft()
  workspaceMode.value = 'browse'
  setJournalFocus(false)
  editorGeneration += 1
  activeDocument.value = null
  editorError.value = null
  await loadNotes()
  await focusSelectedDate()
}

async function deleteActiveNote(): Promise<void> {
  const document = activeDocument.value
  if (saving.value || !document) return
  if (!window.confirm(`确定删除“${document.summary.title || '无标题日志'}”吗？`)) return
  const result = await deleteNote(document.summary.id, document.summary.revision)
  if (result.kind === 'error') {
    editorError.value = result.error.reason
    return
  }
  if (result.result.kind === 'conflict') {
    editorError.value = 'note_revision_conflict'
    return
  }
  if (result.result.kind !== 'deleted') {
    editorError.value = 'journal_delete_not_deleted'
    return
  }
  dirty.value = false
  await closeEditor()
}

async function toggleCompleted(note: NoteSummary): Promise<void> {
  if (saving.value || !['active', 'completed'].includes(note.status)) return
  saving.value = true
  operationError.value = null
  const fetched = await getNote(note.id)
  if (fetched.kind === 'error') {
    operationError.value = fetched.error.reason
    saving.value = false
    return
  }
  const document = fetched.document
  const result = await writeNote({
    kind: 'save',
    id: note.id,
    expectedRevision: document.summary.revision,
    autosave: false,
    meta: {
      title: document.summary.title,
      diaryDate: document.summary.diaryDate,
      tags: document.summary.tags,
      status: document.summary.status === 'completed' ? 'active' : 'completed',
      pinned: document.summary.pinned,
    },
    bodyMarkdown: document.bodyMarkdown,
  })
  saving.value = false
  if (result.kind === 'error') operationError.value = result.error.reason
  else if (result.result.kind === 'conflict') operationError.value = 'note_revision_conflict'
  await loadNotes()
}

async function runFetch(): Promise<void> {
  const input = fetchInput()
  if (runningFetchDate.value) return
  fetchTasks.set(input.localDate, {
    state: 'loading',
    phase: 'collecting',
    error: null,
    collection: null,
    summary: null,
    captureStates: {},
    captureMessages: {},
  })
  const task = fetchTasks.get(input.localDate)!
  const collected = await collectJournalUsage(input)
  if (collected.kind === 'error') {
    task.error = collected.error
    task.state = 'error'
    task.phase = null
    return
  }
  task.collection = collected.collection
  task.phase = 'summarizing'
  const result = await fetchJournalSummary(input)
  if (result.kind === 'error') {
    task.error = result.error
    task.state = 'error'
    task.phase = null
    return
  }
  task.summary = result.summary
  task.state = 'ready'
  task.phase = null
}

function appendFetchedDraft(): void {
  const summary = fetchedSummary.value
  if (!summary) return
  draftBody.value = draftBody.value.trim()
    ? `${draftBody.value.trimEnd()}\n\n---\n\n${summary.markdownBody}`
    : summary.markdownBody
  if (!draftTitle.value.trim() || draftTitle.value === `${selectedDate.value} 工作日志`) {
    draftTitle.value = summary.title
  }
  markDirty()
}

async function captureCandidate(candidate: JournalKnowledgeCandidate): Promise<void> {
  const task = activeFetchTask.value
  if (!task || task.captureStates[candidate.sourceSessionId] === 'loading') return
  if (!window.confirm('将调用“对话知识入库”Skill 写入本地知识库，是否继续？')) return
  task.captureStates[candidate.sourceSessionId] = 'loading'
  const result = await captureJournalKnowledge(fetchInput(), candidate.sourceSessionId, true)
  if (result.kind === 'error') {
    task.captureStates[candidate.sourceSessionId] = 'error'
    task.captureMessages[candidate.sourceSessionId] = result.error.reason
    return
  }
  task.captureStates[candidate.sourceSessionId] = result.result.state === 'stored' ? 'stored' : 'error'
  task.captureMessages[candidate.sourceSessionId] = result.result.state === 'stored'
    ? result.result.notePaths.join('、')
    : result.result.warnings.join('、') || 'knowledge_not_stored'
}

async function focusSelectedDate(): Promise<void> {
  await nextTick()
  calendarGrid.value
    ?.querySelector<HTMLElement>(`[data-calendar-date="${selectedDate.value}"]`)
    ?.focus()
}

function moveSelection(days: number): void {
  const nextDate = addDays(dateFromKey(selectedDate.value), days)
  const nextMonth = startOfMonth(nextDate)
  const monthChanged = nextMonth.getTime() !== visibleMonth.value.getTime()
  selectedDate.value = localDateKey(nextDate)
  if (monthChanged) {
    visibleMonth.value = nextMonth
    void loadNotes()
  }
  void focusSelectedDate()
}

function onCalendarKeydown(event: KeyboardEvent): void {
  const actions: Record<string, () => void> = {
    ArrowLeft: () => moveSelection(-1),
    ArrowRight: () => moveSelection(1),
    ArrowUp: () => moveSelection(-7),
    ArrowDown: () => moveSelection(7),
    Home: () => moveSelection(-mondayIndex(dateFromKey(selectedDate.value))),
    End: () => moveSelection(6 - mondayIndex(dateFromKey(selectedDate.value))),
    PageUp: () => changeMonth(-1),
    PageDown: () => changeMonth(1),
    Enter: () => { void openDay(selectedDate.value) },
  }
  const action = actions[event.key]
  if (!action) return
  event.preventDefault()
  action()
}

onMounted(() => {
  void loadNotes()
})

onActivated(() => {
  if (workspaceMode.value === 'editor') setJournalFocus(true)
})

onDeactivated(() => {
  setJournalFocus(false)
})

onBeforeUnmount(() => {
  setJournalFocus(false)
  active = false
  loadGeneration += 1
  documentGeneration += 1
  editorGeneration += 1
  if (autosaveTimer !== null) window.clearTimeout(autosaveTimer)
})
</script>

<template>
  <section
    class="notes-console notes-calendar-console"
    :class="{
      'is-editor-open': workspaceMode === 'editor',
      'has-editor-error': workspaceMode === 'editor' && editorError,
    }"
    aria-labelledby="notes-heading"
  >
    <h1 id="notes-heading" class="sr-only">日志</h1>

    <template v-if="workspaceMode === 'browse'">
      <header class="notes-calendar-toolbar">
        <div class="notes-calendar-navigation">
          <h2 aria-live="polite">{{ monthLabel }}</h2>
          <div class="notes-calendar-actions">
            <button class="notes-calendar-today" type="button" :disabled="refreshing" @click="goToToday">
              <CalendarDays :size="17" aria-hidden="true" />
              <span>今天</span>
            </button>
            <button type="button" aria-label="上个月" title="上个月" @click="changeMonth(-1)">
              <ChevronLeft :size="20" aria-hidden="true" />
            </button>
            <button type="button" aria-label="下个月" title="下个月" @click="changeMonth(1)">
              <ChevronRight :size="20" aria-hidden="true" />
            </button>
            <button type="button" :aria-label="viewMode === 'calendar' ? '刷新日志日历' : '刷新日志列表'" title="刷新" :disabled="refreshing" @click="loadNotes">
              <RefreshCw :size="17" :class="{ 'is-spinning': refreshing }" aria-hidden="true" />
            </button>
          </div>
        </div>

        <div class="notes-view-switch" role="group" aria-label="日志视图">
          <button type="button" :class="{ 'is-active': viewMode === 'calendar' }" :aria-pressed="viewMode === 'calendar'" @click="setViewMode('calendar')">
            <CalendarDays :size="17" aria-hidden="true" />
            <span>日历</span>
          </button>
          <button type="button" :class="{ 'is-active': viewMode === 'list' }" :aria-pressed="viewMode === 'list'" @click="setViewMode('list')">
            <List :size="17" aria-hidden="true" />
            <span>列表</span>
          </button>
        </div>
      </header>

      <div v-if="pageError" class="notes-calendar-message is-error" role="status">
        <CircleAlert :size="18" aria-hidden="true" />
        <span>{{ viewMode === 'calendar' ? '日志日历不可用' : '日志列表不可用' }}</span>
        <code>{{ pageError.reason }}</code>
        <button type="button" @click="loadNotes">重试</button>
      </div>
      <div v-else-if="partialReason" class="notes-calendar-message is-warning" role="status">
        <CircleAlert :size="18" aria-hidden="true" />
        <span>部分日志正文不可用</span>
        <code>{{ partialReason }}</code>
      </div>
      <div v-else-if="operationError" class="notes-calendar-message is-error" role="status">
        <CircleAlert :size="18" aria-hidden="true" />
        <span>日志更新失败</span>
        <code>{{ operationError }}</code>
      </div>

      <div class="notes-mode-workspace">
        <div v-if="viewMode === 'calendar'" class="notes-calendar-surface">
          <div class="notes-calendar-weekdays" role="row" aria-hidden="true">
            <span v-for="weekday in weekdays" :key="weekday">{{ weekday }}</span>
          </div>

          <div
            ref="calendarGrid"
            class="notes-calendar-grid"
            :class="{ 'is-loading': loading }"
            role="grid"
            :aria-label="`${monthLabel}日志日历`"
            @keydown="onCalendarKeydown"
          >
            <div v-for="(week, weekIndex) in calendarWeeks" :key="weekIndex" class="notes-calendar-week" role="row">
              <div
                v-for="day in week"
                :key="day.date"
                class="notes-calendar-day"
                :class="{
                  'is-adjacent': !day.inCurrentMonth,
                  'is-selected': selectedDate === day.date,
                  'is-today': day.isToday,
                  'has-notes': day.notes.length > 0,
                }"
                role="gridcell"
                :data-calendar-date="day.date"
                :tabindex="selectedDate === day.date ? 0 : -1"
                :aria-selected="selectedDate === day.date"
                :aria-label="`${day.date}${day.isToday ? '，今天' : ''}${day.notes.length > 0 ? '，有日志' : '，无日志'}，双击编辑`"
                @click="selectDate(day.date)"
                @dblclick="openDay(day.date)"
              >
                <span class="notes-calendar-date">{{ day.dayNumber }}</span>
                <span v-if="day.isToday" class="notes-calendar-today-label">今天</span>
                <div class="notes-calendar-memos">
                  <button
                    v-for="note in day.notes"
                    :key="note.id"
                    class="notes-calendar-memo"
                    :class="[`is-${note.status}`, { 'is-completed': isCompleted(note) }]"
                    type="button"
                    :title="memoPreview(note)"
                    :aria-label="`${statusLabel(note.status)}：${memoPreview(note)}，双击编辑`"
                    @click.stop="selectDate(day.date)"
                    @dblclick.stop="openNote(note)"
                  >
                    <Check v-if="isCompleted(note)" :size="13" aria-hidden="true" />
                    <CircleDot v-else :size="13" aria-hidden="true" />
                    <span>{{ memoPreview(note) }}</span>
                  </button>
                </div>
              </div>
            </div>

            <div v-if="loading" class="notes-calendar-overlay" role="status">
              <LoaderCircle :size="28" class="is-spinning" aria-hidden="true" />
              <span>正在读取日志</span>
            </div>
          </div>
        </div>

        <div v-else class="notes-memo-list-wrap" :class="{ 'is-loading': loading }">
          <table class="notes-memo-list">
            <thead>
              <tr>
                <th scope="col">状态</th>
                <th scope="col">时间</th>
                <th scope="col">日志</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="note in sortedNotes"
                :key="note.id"
                :class="{ 'is-completed': isCompleted(note) }"
                @dblclick="openNote(note)"
              >
                <td>
                  <button
                    class="notes-complete-toggle"
                    type="button"
                    :class="{ 'is-completed': isCompleted(note) }"
                    :disabled="saving || !['active', 'completed'].includes(note.status)"
                    :aria-label="isCompleted(note) ? `将${memoPreview(note)}标记为记录中` : `将${memoPreview(note)}标记为已完成`"
                    :aria-pressed="isCompleted(note)"
                    @click.stop="toggleCompleted(note)"
                  >
                    <Check v-if="isCompleted(note)" :size="16" aria-hidden="true" />
                  </button>
                </td>
                <td><time :datetime="new Date(note.updatedAtMs).toISOString()">{{ formatTimestamp(note.updatedAtMs) }}</time></td>
                <td>
                  <button class="notes-list-open" type="button" :title="memoPreview(note)" @dblclick.stop="openNote(note)">
                    <span>{{ memoPreview(note) }}</span>
                    <small>{{ statusLabel(note.status) }}</small>
                  </button>
                </td>
              </tr>
            </tbody>
          </table>

          <div v-if="loading" class="notes-calendar-overlay" role="status">
            <LoaderCircle :size="28" class="is-spinning" aria-hidden="true" />
            <span>正在读取日志</span>
          </div>
          <div v-else-if="notes.length === 0 && !pageError" class="notes-list-empty" role="status">
            <FileText :size="24" aria-hidden="true" />
            <span>暂无日志</span>
          </div>
        </div>
      </div>
    </template>

    <template v-else>
      <header class="journal-editor-header journal-focus-header">
        <button class="journal-back-button" type="button" @click="closeEditor">
          <ArrowLeft :size="18" aria-hidden="true" />
          <span class="sr-only">返回日历</span>
        </button>
        <div class="journal-focus-title">
          <h2>日志工作台</h2>
          <span>{{ selectedDate }}</span>
        </div>
        <div class="journal-editor-actions">
          <span :class="`is-${saveState}`" role="status" aria-live="polite">{{ saveLabel }}</span>
          <button type="button" aria-label="立即保存" title="立即保存" :disabled="saving || !dirty" @click="saveDraft">
            <Save :size="18" aria-hidden="true" />
          </button>
          <button v-if="activeDocument" class="is-danger" type="button" aria-label="删除日志" title="删除日志" :disabled="saving" @click="deleteActiveNote">
            <Trash2 :size="18" aria-hidden="true" />
          </button>
        </div>
      </header>

      <div v-if="editorError" class="journal-editor-message is-error" role="alert">
        <CircleAlert :size="17" aria-hidden="true" />
        <span>日志保存或读取失败</span>
        <code>{{ editorError }}</code>
      </div>

      <div class="journal-editor-layout">
        <aside class="journal-session-rail" aria-labelledby="journal-sessions-heading">
          <header class="journal-session-toolbar">
            <div>
              <MessagesSquare :size="18" aria-hidden="true" />
              <h3 id="journal-sessions-heading">会话列表</h3>
              <span>{{ includedSessionCount }}</span>
            </div>
            <button
              class="journal-fetch-button"
              type="button"
              :aria-label="runningFetchDate ? `${runningFetchDate} 的 AI 会话正在后台获取` : '获取今日 AI 会话和用量'"
              :title="runningFetchDate ? `${runningFetchDate} 正在后台获取` : 'FETCH 今日会话'"
              :disabled="runningFetchDate !== null"
              @click="runFetch"
            >
              <LoaderCircle v-if="runningFetchDate" :size="17" class="is-spinning" aria-hidden="true" />
              <RefreshCw v-else :size="17" aria-hidden="true" />
            </button>
          </header>

          <div class="journal-session-groups">
            <section v-for="group in sourceGroups" :key="group.source" class="journal-session-group">
              <header>
                <Bot v-if="group.source === 'codex'" :size="18" aria-hidden="true" />
                <Sparkles v-else-if="group.source === 'claude'" :size="18" aria-hidden="true" />
                <TerminalSquare v-else :size="18" aria-hidden="true" />
                <strong>{{ group.label }}</strong>
                <span>{{ group.coverage?.includedSessions ?? 0 }}</span>
              </header>
              <ul v-if="group.sessions.length">
                <li v-for="session in group.sessions" :key="session.sessionId" :class="{ 'is-ignored': session.eligibility.state === 'ignored_short' }">
                  <strong :title="session.title">{{ session.title }}</strong>
                  <span>
                    {{ session.messageCount }} 条消息
                    <template v-if="session.eligibility.state === 'ignored_short'"> · 已忽略短会话</template>
                  </span>
                  <code :title="session.workspace ?? session.sessionId">{{ session.workspace ?? session.sessionId }}</code>
                </li>
              </ul>
              <p v-else>
                {{ group.coverage && (group.coverage.ignoredShortSessions ?? 0) > 0
                  ? `${group.coverage.ignoredShortSessions} 个短会话已忽略`
                  : group.coverage?.reason ?? '尚未读取 cc-switch 会话索引' }}
              </p>
            </section>
          </div>

          <section class="journal-usage-summary" aria-labelledby="journal-usage-heading">
            <div>
              <Coins :size="17" aria-hidden="true" />
              <h3 id="journal-usage-heading">AI 使用情况</h3>
              <span v-if="collectedJournal" :class="`is-${collectedJournal.tokenUsage.state}`">{{ collectedJournal.tokenUsage.state }}</span>
            </div>
            <strong>{{ formatCompactNumber(collectedJournal?.tokenUsage.reportedTotalTokens ?? null) }}</strong>
            <small v-if="collectedJournal">
              今日 Token · {{ collectedJournal.tokenUsage.bySource.reduce((total, source) => total + source.requestCount, 0) }} 次请求
            </small>
            <small v-else>点击右上角刷新，读取 cc-switch 实际统计</small>
            <dl v-if="collectedJournal?.tokenUsage.bySource.length">
              <template v-for="usage in collectedJournal.tokenUsage.bySource" :key="usage.source">
                <dt>{{ sourceLabel(usage.source) }}</dt>
                <dd>{{ usage.reportedTotalTokens.toLocaleString('zh-CN') }}</dd>
              </template>
            </dl>
          </section>
        </aside>

        <main class="journal-writing-pane" aria-label="日志编辑">
          <div v-if="editorLoading" class="notes-editor-state" role="status">
            <LoaderCircle :size="24" class="is-spinning" aria-hidden="true" />
            <span>正在读取日志</span>
          </div>
          <template v-else>
            <header class="journal-document-header">
              <div>
                <label class="sr-only" for="journal-title">日志标题</label>
                <input
                  id="journal-title"
                  v-model="draftTitle"
                  class="journal-title-input"
                  type="text"
                  maxlength="512"
                  aria-label="日志标题"
                  @input="markDirty"
                />
                <p>
                  <Clock3 :size="14" aria-hidden="true" />
                  <time :datetime="selectedDate">{{ selectedDate }}</time>
                  <span>Markdown 实时渲染</span>
                </p>
              </div>
            </header>
            <JournalMarkdownEditor
              ref="markdownEditor"
              :model-value="draftBody"
              :disabled="saving"
              @update:model-value="updateBody"
            />
          </template>
        </main>

        <aside class="journal-fetch-inspector" aria-labelledby="journal-fetch-heading">
          <header>
            <ListTree :size="17" aria-hidden="true" />
            <h2 id="journal-fetch-heading">日志大纲</h2>
          </header>

          <nav class="journal-outline" aria-label="日志 Markdown 大纲">
            <JournalOutlineTree
              v-if="journalOutline.length"
              :items="journalOutline"
              @select="focusOutlineHeading"
            />
            <p v-else>输入 Markdown 标题后，这里会生成大纲。</p>
          </nav>

          <section class="journal-summary-panel" aria-labelledby="journal-summary-heading">
            <header>
              <Sparkles :size="17" aria-hidden="true" />
              <h3 id="journal-summary-heading">今日工作总结</h3>
            </header>
            <div v-if="fetchState === 'idle'" class="journal-summary-state">
              <span>尚未获取</span>
              <small v-if="runningFetchDate && runningFetchDate !== selectedDate">
                {{ runningFetchDate }} 的总结正在后台获取，完成后可返回该日期查看。
              </small>
              <small v-else>左侧刷新会读取会话、Token 并生成总结。</small>
            </div>
            <div v-else-if="fetchState === 'loading'" class="journal-summary-state" role="status">
              <LoaderCircle :size="20" class="is-spinning" aria-hidden="true" />
              <span>{{ fetchPhase === 'collecting' ? '正在读取 cc-switch' : '正在生成工作总结' }}</span>
              <small>{{ fetchPhase === 'collecting' ? '先取得会话与 Token 事实' : '会话与 Token 已读取，短会话已过滤' }}</small>
            </div>
            <div v-else-if="fetchState === 'error' && fetchError" class="journal-summary-state is-error" role="alert">
              <CircleAlert :size="20" aria-hidden="true" />
              <span>{{ collectedJournal ? 'AI 总结失败，用量数据已保留' : '会话与用量读取失败' }}</span>
              <code>{{ fetchError.reason }}</code>
              <button type="button" @click="runFetch">重试</button>
            </div>
            <template v-else-if="fetchedSummary">
              <p>{{ fetchedSummary.workItems.length }} 个工作项 · {{ fetchedSummary.knowledgeItems.length }} 条知识</p>
              <button class="journal-append-button" type="button" @click="appendFetchedDraft">
                <Save :size="16" aria-hidden="true" />
                <span>追加到日志</span>
              </button>
            </template>
          </section>

          <section v-if="fetchedSummary" class="journal-inspector-section" aria-labelledby="journal-knowledge-heading">
            <div class="journal-section-heading">
              <BookOpenCheck :size="17" aria-hidden="true" />
              <h3 id="journal-knowledge-heading">知识入库候选</h3>
              <span>{{ knowledgeCandidateCount }} 个候选</span>
            </div>

            <div v-if="knowledgeEligibilityRows.length" class="journal-knowledge-list" aria-label="知识入库判定">
              <article v-for="(item, index) in knowledgeEligibilityRows" :key="`${item.topic}:${item.sourceSessionIds.join(':')}:${index}`" class="journal-knowledge-item">
                <header>
                  <strong>{{ item.topic }}</strong>
                  <span :class="item.eligible ? 'is-eligible' : 'is-ineligible'">
                    <Check v-if="item.eligible" :size="13" aria-hidden="true" />
                    <CircleAlert v-else :size="13" aria-hidden="true" />
                    {{ item.eligible ? '可入库' : '暂不可入库' }}
                  </span>
                </header>
                <p>{{ item.summary }}</p>
                <small class="journal-knowledge-reason">{{ item.reason }}</small>
                <small
                  v-if="item.candidate && captureMessages[item.candidate.sourceSessionId]"
                  class="journal-knowledge-capture-message"
                  :class="{ 'is-error': captureStates[item.candidate.sourceSessionId] === 'error' }"
                >
                  {{ captureMessages[item.candidate.sourceSessionId] }}
                </small>
                <button
                  v-if="item.candidate"
                  class="journal-knowledge-capture-button"
                  type="button"
                  :disabled="captureStates[item.candidate.sourceSessionId] === 'loading' || captureStates[item.candidate.sourceSessionId] === 'stored'"
                  :aria-label="`${captureStates[item.candidate.sourceSessionId] === 'stored' ? '已将' : '确认将'}“${item.topic}”的来源会话入库`"
                  @click="captureCandidate(item.candidate)"
                >
                  <LoaderCircle v-if="captureStates[item.candidate.sourceSessionId] === 'loading'" :size="15" class="is-spinning" aria-hidden="true" />
                  <BookOpenCheck v-else :size="15" aria-hidden="true" />
                  <span>{{ captureStates[item.candidate.sourceSessionId] === 'stored' ? '已入库' : '确认入库' }}</span>
                </button>
              </article>
            </div>
            <p v-else class="journal-knowledge-empty-copy">今日总结未提取到可复用知识。</p>

            <div v-if="unmatchedRecommendedCandidates.length" class="journal-knowledge-candidates">
              <div v-for="candidate in unmatchedRecommendedCandidates" :key="candidate.sourceSessionId" class="journal-knowledge-candidate">
                <code :title="candidate.sourceSessionId">{{ candidate.sourceSessionId }}</code>
                <p>{{ candidate.reason }}</p>
                <button
                  type="button"
                  :disabled="captureStates[candidate.sourceSessionId] === 'loading' || captureStates[candidate.sourceSessionId] === 'stored'"
                  :aria-label="`${captureStates[candidate.sourceSessionId] === 'stored' ? '已将' : '确认将'}会话 ${candidate.sourceSessionId} 入库`"
                  @click="captureCandidate(candidate)"
                >
                  <LoaderCircle v-if="captureStates[candidate.sourceSessionId] === 'loading'" :size="15" class="is-spinning" aria-hidden="true" />
                  <BookOpenCheck v-else :size="15" aria-hidden="true" />
                  <span>{{ captureStates[candidate.sourceSessionId] === 'stored' ? '已入库' : '确认入库' }}</span>
                </button>
                <small v-if="captureMessages[candidate.sourceSessionId]" :class="{ 'is-error': captureStates[candidate.sourceSessionId] === 'error' }">{{ captureMessages[candidate.sourceSessionId] }}</small>
              </div>
            </div>
            <div v-if="knowledgeCandidateCount === 0" class="journal-knowledge-empty" role="status">
              <CircleAlert :size="16" aria-hidden="true" />
              <div>
                <strong>暂无可入库候选</strong>
                <p>知识已保留在日志总结中；只有满足会话级入库条件时才会显示确认按钮。</p>
              </div>
            </div>
          </section>

          <section v-if="collectedJournal" class="journal-source-facts" aria-labelledby="journal-source-heading">
            <header>
              <Folder :size="17" aria-hidden="true" />
              <h3 id="journal-source-heading">采集事实</h3>
            </header>
            <ul>
              <li v-for="source in collectedJournal.sourceCoverage" :key="source.source">
                <span>{{ sourceLabel(source.source) }}</span>
                <strong :class="`is-${source.state}`">{{ source.includedSessions ?? '未知' }} / {{ source.scannedSessions ?? '未知' }}</strong>
                <code :title="source.reason">{{ source.reason }}</code>
              </li>
            </ul>
            <small>同步 {{ formatTimestamp(collectedJournal.tokenUsage.lastSyncedAtMs) }}</small>
          </section>
        </aside>
      </div>
    </template>
  </section>
</template>
