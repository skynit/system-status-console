<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  CalendarDays,
  Check,
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  CircleDot,
  FileText,
  List,
  LoaderCircle,
  RefreshCw,
  Save,
  Trash2,
  X,
} from 'lucide-vue-next'

import { deleteNote, getBackendHealth, getNote, listNotes, writeNote } from '../backend'
import type { BridgeError, NoteDocument, NoteQuery, NoteStatus, NoteSummary } from '../types'

type ViewMode = 'calendar' | 'list'
type EditorMode = 'view' | 'create'
type CalendarDay = {
  date: string
  dayNumber: number
  inCurrentMonth: boolean
  isToday: boolean
  notes: NoteSummary[]
}

const QUERY_LIMIT = 64
const CALENDAR_CELL_COUNT = 42
const weekdays = ['周一', '周二', '周三', '周四', '周五', '周六', '周日']

const today = startOfDay(new Date())
const todayKey = localDateKey(today)
const viewMode = ref<ViewMode>('calendar')
const visibleMonth = ref(startOfMonth(today))
const selectedDate = ref(todayKey)
const notes = ref<NoteSummary[]>([])
const documents = ref<Record<string, NoteDocument>>({})
const loading = ref(true)
const refreshing = ref(false)
const pageError = ref<BridgeError | null>(null)
const partialReason = ref<string | null>(null)
const editorMode = ref<EditorMode | null>(null)
const activeDocument = ref<NoteDocument | null>(null)
const editorLoading = ref(false)
const editorError = ref<string | null>(null)
const operationError = ref<string | null>(null)
const saving = ref(false)
const draftTitle = ref('')
const draftBody = ref('')
const calendarGrid = ref<HTMLElement | null>(null)
let active = true
let loadGeneration = 0
let documentGeneration = 0

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

async function hydrateDocuments(summaries: NoteSummary[], generation: number): Promise<void> {
  const nextDocuments: Record<string, NoteDocument> = {}
  let failedReason: string | null = null
  let nextIndex = 0
  const workers = Array.from({ length: Math.min(6, summaries.length) }, async () => {
    while (nextIndex < summaries.length) {
      const summary = summaries[nextIndex]
      nextIndex += 1
      const result = await getNote(summary.id)
      if (result.kind === 'document') {
        nextDocuments[summary.id] = result.document
      } else if (failedReason === null) {
        failedReason = result.error.reason
      }
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
  if (!error && collected.length > 0) {
    await hydrateDocuments(collected, hydrationGeneration)
  }
  if (!active || generation !== loadGeneration) return
  loading.value = false
}

function setViewMode(mode: ViewMode): void {
  if (viewMode.value === mode) return
  viewMode.value = mode
  closeEditor()
  void loadNotes()
}

function changeMonth(amount: number): void {
  const nextMonth = addMonths(visibleMonth.value, amount)
  visibleMonth.value = nextMonth
  selectedDate.value = nextMonth.getFullYear() === today.getFullYear()
    && nextMonth.getMonth() === today.getMonth()
    ? todayKey
    : localDateKey(nextMonth)
  closeEditor()
  void loadNotes()
  void focusSelectedDate()
}

function goToToday(): void {
  visibleMonth.value = startOfMonth(today)
  selectedDate.value = todayKey
  closeEditor()
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
  return '未完成'
}

function isCompleted(note: NoteSummary): boolean {
  return note.status === 'completed'
}

function formatTimestamp(timestamp: number): string {
  return new Intl.DateTimeFormat('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(timestamp).replaceAll('/', '-')
}

async function openNote(note: NoteSummary): Promise<void> {
  editorMode.value = 'view'
  editorLoading.value = true
  editorError.value = null
  activeDocument.value = documents.value[note.id] ?? null
  const result = await getNote(note.id)
  if (result.kind === 'document') {
    activeDocument.value = result.document
    documents.value = { ...documents.value, [note.id]: result.document }
  } else {
    editorError.value = result.error.reason
  }
  editorLoading.value = false
}

function openCreate(date: string): void {
  selectedDate.value = date
  editorMode.value = 'create'
  activeDocument.value = null
  editorError.value = null
  draftTitle.value = ''
  draftBody.value = ''
  void nextTick(() => document.querySelector<HTMLInputElement>('#new-note-title')?.focus())
}

function closeEditor(): void {
  editorMode.value = null
  activeDocument.value = null
  editorError.value = null
  editorLoading.value = false
}

async function createNote(): Promise<void> {
  if (saving.value) return
  if (!draftTitle.value.trim() && !draftBody.value.trim()) {
    editorError.value = 'note_content_required'
    return
  }
  saving.value = true
  editorError.value = null
  const result = await writeNote({
    kind: 'create',
    meta: {
      title: draftTitle.value.trim() || draftBody.value.trim().split(/\r?\n/, 1)[0].slice(0, 512),
      diaryDate: selectedDate.value,
      tags: [],
      status: 'active',
      pinned: false,
    },
    bodyMarkdown: draftBody.value,
  })
  saving.value = false
  if (result.kind === 'error') {
    editorError.value = result.error.reason
    return
  }
  if (result.result.kind !== 'stored') {
    editorError.value = 'note_create_not_stored'
    return
  }
  closeEditor()
  await loadNotes()
}

async function toggleCompleted(note: NoteSummary, fromEditor = false): Promise<void> {
  if (saving.value || !['active', 'completed'].includes(note.status)) return
  saving.value = true
  if (fromEditor) editorError.value = null
  else operationError.value = null
  const fetched = await getNote(note.id)
  if (fetched.kind === 'error') {
    if (fromEditor) editorError.value = fetched.error.reason
    else operationError.value = fetched.error.reason
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
  if (result.kind === 'error') {
    if (fromEditor) editorError.value = result.error.reason
    else operationError.value = result.error.reason
    return
  }
  if (result.result.kind === 'conflict') {
    if (fromEditor) editorError.value = 'note_revision_conflict'
    else operationError.value = 'note_revision_conflict'
    return
  }
  if (result.result.kind !== 'stored') return
  if (fromEditor && activeDocument.value?.summary.id === note.id) {
    activeDocument.value = {
      summary: result.result.note,
      bodyMarkdown: document.bodyMarkdown,
    }
  }
  await loadNotes()
}

async function deleteActiveNote(): Promise<void> {
  const document = activeDocument.value
  if (saving.value || !document) return
  if (!window.confirm(`确定删除“${document.summary.title || '无标题备忘录'}”吗？`)) return
  saving.value = true
  editorError.value = null
  const result = await deleteNote(document.summary.id, document.summary.revision)
  saving.value = false
  if (result.kind === 'error') {
    editorError.value = result.error.reason
    return
  }
  if (result.result.kind === 'conflict') {
    editorError.value = 'note_revision_conflict'
    return
  }
  if (result.result.kind !== 'deleted') {
    editorError.value = 'note_delete_not_deleted'
    return
  }
  closeEditor()
  await loadNotes()
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
    closeEditor()
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
    Enter: () => openCreate(selectedDate.value),
  }
  const action = actions[event.key]
  if (!action) return
  event.preventDefault()
  action()
}

onMounted(() => {
  void loadNotes()
})

onBeforeUnmount(() => {
  active = false
  loadGeneration += 1
  documentGeneration += 1
})
</script>

<template>
  <section class="notes-console notes-calendar-console" aria-labelledby="notes-heading">
    <h1 id="notes-heading" class="sr-only">备忘录</h1>

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
          <button type="button" :aria-label="viewMode === 'calendar' ? '刷新日历' : '刷新列表'" title="刷新" :disabled="refreshing" @click="loadNotes">
            <RefreshCw :size="17" :class="{ 'is-spinning': refreshing }" aria-hidden="true" />
          </button>
        </div>
      </div>

      <div class="notes-view-switch" role="group" aria-label="备忘录视图">
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
      <span>{{ viewMode === 'calendar' ? '备忘录日历不可用' : '备忘录列表不可用' }}</span>
      <code>{{ pageError.reason }}</code>
      <button type="button" @click="loadNotes">重试</button>
    </div>
    <div v-else-if="partialReason" class="notes-calendar-message is-warning" role="status">
      <CircleAlert :size="18" aria-hidden="true" />
      <span>部分备忘录正文不可用</span>
      <code>{{ partialReason }}</code>
    </div>
    <div v-else-if="operationError" class="notes-calendar-message is-error" role="status">
      <CircleAlert :size="18" aria-hidden="true" />
      <span>备忘录更新失败</span>
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
          :aria-label="`${monthLabel}备忘录日历`"
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
              :aria-label="`${day.date}${day.isToday ? '，今天' : ''}${day.notes.length > 0 ? `，有备忘录` : '，无备忘录'}，双击新建`"
              @click="selectDate(day.date)"
              @dblclick="openCreate(day.date)"
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
                  :aria-label="`${statusLabel(note.status)}：${memoPreview(note)}`"
                  @click.stop="openNote(note)"
                  @dblclick.stop
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
            <span>正在读取备忘录</span>
          </div>
        </div>
      </div>

      <div v-else class="notes-memo-list-wrap" :class="{ 'is-loading': loading }">
        <table class="notes-memo-list">
          <thead>
            <tr>
              <th scope="col">完成</th>
              <th scope="col">时间</th>
              <th scope="col">备忘录</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="note in sortedNotes"
              :key="note.id"
              :class="{ 'is-completed': isCompleted(note), 'is-selected': activeDocument?.summary.id === note.id }"
              @click="openNote(note)"
            >
              <td>
                <button
                  class="notes-complete-toggle"
                  type="button"
                  :class="{ 'is-completed': isCompleted(note) }"
                  :disabled="saving || !['active', 'completed'].includes(note.status)"
                  :aria-label="isCompleted(note) ? `将${memoPreview(note)}标记为未完成` : `将${memoPreview(note)}标记为已完成`"
                  :aria-pressed="isCompleted(note)"
                  @click.stop="toggleCompleted(note)"
                >
                  <Check v-if="isCompleted(note)" :size="16" aria-hidden="true" />
                </button>
              </td>
              <td><time :datetime="new Date(note.updatedAtMs).toISOString()">{{ formatTimestamp(note.updatedAtMs) }}</time></td>
              <td>
                <button class="notes-list-open" type="button" :title="memoPreview(note)" @click.stop="openNote(note)">
                  <span>{{ memoPreview(note) }}</span>
                  <small>{{ statusLabel(note.status) }}</small>
                </button>
              </td>
            </tr>
          </tbody>
        </table>

        <div v-if="loading" class="notes-calendar-overlay" role="status">
          <LoaderCircle :size="28" class="is-spinning" aria-hidden="true" />
          <span>正在读取备忘录</span>
        </div>
        <div v-else-if="notes.length === 0 && !pageError" class="notes-list-empty" role="status">
          <FileText :size="24" aria-hidden="true" />
          <span>暂无备忘录</span>
        </div>
      </div>

    </div>

    <div v-if="editorMode" class="notes-modal-backdrop" @mousedown.self="closeEditor">
        <section
          class="notes-memo-editor"
          role="dialog"
          aria-modal="true"
          :aria-label="editorMode === 'create' ? '添加备忘录' : '查看备忘录'"
          @keydown.esc.stop="closeEditor"
        >
          <header>
            <div>
              <strong>{{ editorMode === 'create' ? '添加备忘录' : '备忘录内容' }}</strong>
              <small>{{ editorMode === 'create' ? selectedDate : activeDocument?.summary.diaryDate }}</small>
            </div>
            <button type="button" aria-label="关闭备忘录弹窗" title="关闭" @click="closeEditor">
              <X :size="18" aria-hidden="true" />
            </button>
          </header>

          <div v-if="editorLoading" class="notes-editor-state" role="status">
            <LoaderCircle :size="22" class="is-spinning" aria-hidden="true" />
            <span>正在读取正文</span>
          </div>

          <form v-else-if="editorMode === 'create'" class="notes-create-form" @submit.prevent="createNote">
            <label>
              <span>标题</span>
              <input id="new-note-title" v-model="draftTitle" type="text" maxlength="512" placeholder="备忘录标题" />
            </label>
            <label class="notes-create-body">
              <span>正文</span>
              <textarea v-model="draftBody" placeholder="输入备忘录内容"></textarea>
            </label>
            <p v-if="editorError" class="notes-editor-error" role="alert"><code>{{ editorError }}</code></p>
            <button class="notes-editor-save" type="submit" :disabled="saving">
              <Save :size="16" aria-hidden="true" />
              <span>{{ saving ? '正在保存' : '保存' }}</span>
            </button>
          </form>

          <div v-else-if="activeDocument" class="notes-document-view">
            <button
              class="notes-document-status"
              type="button"
              :class="{ 'is-completed': isCompleted(activeDocument.summary) }"
              :disabled="saving || !['active', 'completed'].includes(activeDocument.summary.status)"
              :aria-label="isCompleted(activeDocument.summary) ? '标记为未完成' : '标记为已完成'"
              :aria-pressed="isCompleted(activeDocument.summary)"
              @click="toggleCompleted(activeDocument.summary, true)"
            >
              <Check v-if="isCompleted(activeDocument.summary)" :size="15" aria-hidden="true" />
              <CircleDot v-else :size="15" aria-hidden="true" />
              <span>{{ statusLabel(activeDocument.summary.status) }}</span>
            </button>
            <h3>{{ activeDocument.summary.title }}</h3>
            <pre>{{ activeDocument.bodyMarkdown || '无正文' }}</pre>
            <p v-if="editorError" class="notes-editor-error" role="alert"><code>{{ editorError }}</code></p>
            <div class="notes-document-actions">
              <button class="notes-document-delete" type="button" :disabled="saving" @click="deleteActiveNote">
                <Trash2 :size="16" aria-hidden="true" />
                <span>{{ saving ? '正在处理' : '删除备忘录' }}</span>
              </button>
            </div>
          </div>

          <div v-else-if="editorError" class="notes-editor-state is-error" role="alert">
            <CircleAlert :size="22" aria-hidden="true" />
            <code>{{ editorError }}</code>
          </div>
        </section>
    </div>
  </section>
</template>
