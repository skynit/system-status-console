import { flushPromises, mount } from '@vue/test-utils'
import { defineComponent, h, KeepAlive, nextTick, shallowRef, type Component } from 'vue'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  captureJournalKnowledge,
  collectJournalUsage,
  deleteNote,
  fetchJournalSummary,
  getBackendHealth,
  getNote,
  listNotes,
  writeNote,
} from './backend'
import JournalMarkdownEditor from './components/JournalMarkdownEditor.vue'
import MemosView from './views/MemosView.vue'
import type { JournalCollection, JournalSummary, NoteDocument, NoteSummary } from './types'

vi.mock('./backend', () => ({
  captureJournalKnowledge: vi.fn(),
  collectJournalUsage: vi.fn(),
  deleteNote: vi.fn(),
  fetchJournalSummary: vi.fn(),
  getBackendHealth: vi.fn(),
  getNote: vi.fn(),
  listNotes: vi.fn(),
  writeNote: vi.fn(),
}))

const mockedHealth = vi.mocked(getBackendHealth)
const mockedGet = vi.mocked(getNote)
const mockedList = vi.mocked(listNotes)
const mockedDelete = vi.mocked(deleteNote)
const mockedWrite = vi.mocked(writeNote)
const mockedFetch = vi.mocked(fetchJournalSummary)
const mockedCapture = vi.mocked(captureJournalKnowledge)
const mockedCollect = vi.mocked(collectJournalUsage)

function summary(overrides: Partial<NoteSummary> = {}): NoteSummary {
  return {
    id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
    title: '发布检查',
    diaryDate: '2026-08-11',
    tags: ['release'],
    status: 'active',
    pinned: true,
    createdAtMs: 1_786_154_300_000,
    updatedAtMs: 1_786_154_400_000,
    deletedAtMs: null,
    revision: 2,
    bodyBytes: 12,
    bodySha256: 'a'.repeat(64),
    ...overrides,
  }
}

function document(note = summary(), bodyMarkdown = '检查发布包签名'): NoteDocument {
  return { summary: note, bodyMarkdown }
}

function page(notes: NoteSummary[], hasMore = false, nextOffset: number | null = null) {
  return {
    query: {
      search: null,
      diaryDateFrom: '2026-08-01',
      diaryDateTo: '2026-08-31',
      tags: [],
      status: null,
      deleted: 'exclude' as const,
      sort: 'diary_date_desc' as const,
      limit: 64,
      offset: 0,
    },
    notes,
    hasMore,
    nextOffset,
  }
}

function fetchedSummary(): JournalSummary {
  return {
    schemaVersion: 1,
    localDate: '2026-08-13',
    timezone: 'Asia/Shanghai',
    title: '2026-08-13 工作日志',
    markdownBody: '# 2026-08-13 工作日志\n\n## 今日工作\n\n- 完成日志编辑页',
    workItems: [{
      workstream: '本机控制台',
      state: 'completed',
      summary: '完成日志编辑页',
      evidence: ['desktop-ui package tests'],
      sourceSessionIds: ['session-1'],
    }],
    knowledgeItems: [{
      topic: 'Markdown',
      summary: '渲染面可直接编辑',
      sourceSessionIds: ['session-1'],
    }],
    knowledgeCandidates: [{
      sourceSessionId: 'session-1',
      recommended: true,
      reason: 'long_knowledge_session',
      recommendedSkill: 'capture-conversations-to-vault',
    }],
    remainingItems: [],
    sourceCoverage: [{
      source: 'codex',
      state: 'healthy',
      reason: 'session_source_ready',
      scannedSessions: 2,
      includedSessions: 1,
      ignoredShortSessions: 1,
    }],
    tokenUsage: {
      state: 'healthy',
      reason: 'cc_switch_usage_ready',
      windowStartMs: new Date(2026, 7, 13).getTime(),
      windowEndMs: new Date(2026, 7, 14).getTime(),
      lastSyncedAtMs: new Date(2026, 7, 13, 18).getTime(),
      inputTokens: 100,
      outputTokens: 20,
      cacheReadTokens: 10,
      cacheCreationTokens: 0,
      reportedTotalTokens: 120,
      totalMethod: 'input_plus_output',
      bySource: [{
        source: 'codex',
        requestCount: 2,
        inputTokens: 100,
        outputTokens: 20,
        cacheReadTokens: 10,
        cacheCreationTokens: 0,
        reportedTotalTokens: 120,
      }],
    },
    warnings: [],
  }
}

function collectedJournal(): JournalCollection {
  const summary = fetchedSummary()
  return {
    schemaVersion: 1,
    localDate: summary.localDate,
    timezone: summary.timezone,
    sourceCoverage: summary.sourceCoverage,
    tokenUsage: summary.tokenUsage,
    sessions: [{
      source: 'codex',
      sessionId: 'session-1',
      title: '完成日志编辑页',
      workspace: '/home/skynit/workspace/sky',
      updatedAtMs: new Date(2026, 7, 13, 18).getTime(),
      eligibility: {
        state: 'included',
        reason: 'substantive_session',
        substantiveMessages: 8,
        contentChars: 4000,
        lengthClass: 'long',
      },
      messageCount: 12,
    }],
    warnings: [],
  }
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void
  const promise = new Promise<T>((settle) => { resolve = settle })
  return { promise, resolve }
}

describe('Journal calendar and editor', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(2026, 7, 12, 10, 0, 0))
    mockedHealth.mockReset()
    mockedGet.mockReset()
    mockedList.mockReset()
    mockedDelete.mockReset()
    mockedWrite.mockReset()
    mockedFetch.mockReset()
    mockedCapture.mockReset()
    mockedCollect.mockReset()
    window.document.documentElement.classList.remove('journal-focus-mode')
    mockedHealth.mockResolvedValue({ status: 'healthy', capabilityReason: 'notes_store_available' })
    mockedList.mockResolvedValue({ kind: 'page', page: page([]) })
    mockedGet.mockImplementation(async (id) => ({
      kind: 'document',
      document: document(summary({ id })),
    }))
    mockedWrite.mockResolvedValue({
      kind: 'mutation',
      result: { kind: 'stored', note: summary({ revision: 3 }) },
    })
    mockedDelete.mockResolvedValue({
      kind: 'mutation',
      result: { kind: 'deleted', note: summary({ revision: 3, deletedAtMs: 1_786_154_500_000 }) },
    })
    mockedFetch.mockResolvedValue({ kind: 'summary', summary: fetchedSummary() })
    mockedCollect.mockResolvedValue({ kind: 'collection', collection: collectedJournal() })
    mockedCapture.mockResolvedValue({
      kind: 'capture',
      result: {
        schemaVersion: 1,
        sessionId: 'session-1',
        state: 'stored',
        notePaths: ['/home/skynit/Uni/ming/30-知识/Markdown.md'],
        warnings: [],
      },
    })
  })

  it('keeps the six-week calendar as the default preview', async () => {
    const wrapper = mount(MemosView)
    await flushPromises()

    expect(wrapper.text()).toContain('2026年8月')
    expect(wrapper.findAll('.notes-calendar-week')).toHaveLength(6)
    expect(wrapper.findAll('.notes-calendar-day')).toHaveLength(42)
    expect(wrapper.get('[data-calendar-date="2026-08-12"]').classes()).toContain('is-today')
    expect(wrapper.find('.journal-editor-layout').exists()).toBe(false)
    expect(wrapper.get('.notes-view-switch').attributes('aria-label')).toBe('日志视图')
    wrapper.unmount()
  })

  it('shows real body previews without opening the editor on one click', async () => {
    const note = summary()
    mockedList.mockResolvedValue({ kind: 'page', page: page([note]) })
    mockedGet.mockResolvedValue({ kind: 'document', document: document(note, '完整正文内容') })
    const wrapper = mount(MemosView)
    await flushPromises()

    const preview = wrapper.get('.notes-calendar-memo')
    expect(preview.text()).toContain('完整正文内容')
    await preview.trigger('click')
    expect(wrapper.find('.journal-editor-layout').exists()).toBe(false)
    wrapper.unmount()
  })

  it('enters the rendered Markdown editor only after double-clicking a day', async () => {
    const note = summary()
    mockedList.mockResolvedValue({ kind: 'page', page: page([note]) })
    mockedGet.mockResolvedValue({ kind: 'document', document: document(note, '# 完整正文内容') })
    const wrapper = mount(MemosView)
    await flushPromises()

    await wrapper.get('[data-calendar-date="2026-08-11"]').trigger('dblclick')
    await flushPromises()
    expect(wrapper.find('.notes-calendar-grid').exists()).toBe(false)
    expect(wrapper.get('.journal-editor-layout').exists()).toBe(true)
    expect(wrapper.get('.journal-session-rail').attributes('aria-labelledby')).toBe('journal-sessions-heading')
    expect(window.document.documentElement.classList.contains('journal-focus-mode')).toBe(true)
    expect(wrapper.get('.journal-markdown-surface').text()).toContain('完整正文内容')
    expect(wrapper.find('textarea').exists()).toBe(false)
    expect(wrapper.text()).toContain('日志大纲')
    expect(wrapper.text()).toContain('AI 使用情况')
    expect(wrapper.text()).toContain('Codex')
    expect(wrapper.text()).toContain('Claude')
    expect(wrapper.text()).toContain('OpenCode')
    expect(wrapper.text()).toContain('读取 cc-switch 实际统计')
    wrapper.unmount()
  })

  it('renders Markdown headings as a hierarchical outline and navigates to them', async () => {
    const note = summary()
    const focusHeading = vi.fn()
    const editorStub = defineComponent({
      name: 'JournalMarkdownEditor',
      props: ['modelValue', 'disabled'],
      emits: ['update:modelValue'],
      setup(_props, { expose }) {
        expose({ focusHeading })
        return () => h('div', { class: 'journal-markdown-surface' })
      },
    })
    mockedList.mockResolvedValue({ kind: 'page', page: page([note]) })
    mockedGet.mockResolvedValue({
      kind: 'document',
      document: document(note, '# 工作日志\n\n## 今日工作\n\n### 本机控制台\n\n## 待办'),
    })
    const wrapper = mount(MemosView, {
      global: { stubs: { JournalMarkdownEditor: editorStub } },
    })
    await flushPromises()
    await wrapper.get('[data-calendar-date="2026-08-11"]').trigger('dblclick')
    await flushPromises()

    const outline = wrapper.get('.journal-outline')
    expect(outline.attributes('aria-label')).toBe('日志 Markdown 大纲')
    expect(outline.findAll('.journal-outline-list')).toHaveLength(3)
    expect(outline.findAll('button').map((button) => button.text())).toEqual([
      '工作日志',
      '今日工作',
      '本机控制台',
      '待办',
    ])
    expect(outline.text()).not.toMatch(/\b1\b|\b3\b|\b5\b/)

    const target = outline.findAll('button')[2]
    expect(target.attributes('data-heading-index')).toBe('2')
    await target.trigger('click')
    expect(focusHeading).toHaveBeenCalledWith(2)
    wrapper.unmount()
  })

  it('provides Enter as the keyboard equivalent of a calendar double-click', async () => {
    const wrapper = mount(MemosView, { attachTo: document.body })
    await flushPromises()
    const today = wrapper.get('[data-calendar-date="2026-08-12"]')
    ;(today.element as HTMLElement).focus()

    await today.trigger('keydown', { key: 'ArrowRight' })
    await flushPromises()
    expect(wrapper.get('[data-calendar-date="2026-08-13"]').attributes('tabindex')).toBe('0')
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('keydown', { key: 'Enter' })
    expect(wrapper.get('.journal-editor-layout').exists()).toBe(true)
    expect(wrapper.get('.journal-document-header time').text()).toBe('2026-08-13')
    wrapper.unmount()
  })

  it('autosaves a new rendered Markdown journal through the existing notes contract', async () => {
    const wrapper = mount(MemosView)
    await flushPromises()
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('dblclick')
    await wrapper.get('#journal-title').setValue('新的工作日志')
    wrapper.findComponent(JournalMarkdownEditor).vm.$emit('update:modelValue', '## 今日工作\n\n完成正文')
    await vi.advanceTimersByTimeAsync(901)
    await flushPromises()

    expect(mockedWrite).toHaveBeenCalledWith({
      kind: 'create',
      meta: {
        title: '新的工作日志',
        diaryDate: '2026-08-13',
        tags: [],
        status: 'active',
        pinned: false,
      },
      bodyMarkdown: '## 今日工作\n\n完成正文',
    })
    expect(wrapper.text()).toContain('已保存')
    wrapper.unmount()
  })

  it('keeps a fetched summary in the inspector until it is explicitly appended', async () => {
    const wrapper = mount(MemosView)
    await flushPromises()
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('dblclick')
    await wrapper.get('.journal-fetch-button').trigger('click')
    await flushPromises()

    expect(mockedCollect).toHaveBeenCalledWith(expect.objectContaining({ localDate: '2026-08-13' }))
    expect(mockedFetch).toHaveBeenCalledWith(expect.objectContaining({ localDate: '2026-08-13' }))
    expect(wrapper.text()).toContain('120')
    expect(wrapper.text()).toContain('1 / 2')
    expect(wrapper.text()).toContain('完成日志编辑页')
    expect(wrapper.findComponent(JournalMarkdownEditor).props('modelValue')).toBe('')

    await wrapper.get('.journal-append-button').trigger('click')
    expect(wrapper.findComponent(JournalMarkdownEditor).props('modelValue')).toContain('完成日志编辑页')
    wrapper.unmount()
  })

  it('continues collection and summarization while the cached journal page is inactive', async () => {
    const collected = deferred<Awaited<ReturnType<typeof collectJournalUsage>>>()
    const summarized = deferred<Awaited<ReturnType<typeof fetchJournalSummary>>>()
    mockedCollect.mockReturnValueOnce(collected.promise)
    mockedFetch.mockReturnValueOnce(summarized.promise)
    const OtherPage = defineComponent({
      name: 'OtherPage',
      setup: () => () => h('div', { class: 'other-page' }),
    })
    const activeComponent = shallowRef<Component>(MemosView)
    const Host = defineComponent({
      setup: () => () => h(KeepAlive, null, {
        default: () => h(activeComponent.value),
      }),
    })
    const wrapper = mount(Host)
    await flushPromises()
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('dblclick')
    await wrapper.get('.journal-fetch-button').trigger('click')

    expect(wrapper.get('.journal-fetch-button').attributes()).toHaveProperty('disabled')
    expect(window.document.documentElement.classList.contains('journal-focus-mode')).toBe(true)

    activeComponent.value = OtherPage
    await nextTick()
    expect(wrapper.get('.other-page').exists()).toBe(true)
    expect(window.document.documentElement.classList.contains('journal-focus-mode')).toBe(false)

    collected.resolve({ kind: 'collection', collection: collectedJournal() })
    await flushPromises()
    expect(mockedFetch).toHaveBeenCalledWith(expect.objectContaining({ localDate: '2026-08-13' }))
    summarized.resolve({ kind: 'summary', summary: fetchedSummary() })
    await flushPromises()

    activeComponent.value = MemosView
    await nextTick()
    expect(wrapper.text()).toContain('1 个工作项 · 1 条知识')
    expect(wrapper.get('.journal-fetch-button').attributes()).not.toHaveProperty('disabled')
    expect(window.document.documentElement.classList.contains('journal-focus-mode')).toBe(true)
    wrapper.unmount()
  })

  it('keeps a background fetch attached to its date after returning to the calendar', async () => {
    const collected = deferred<Awaited<ReturnType<typeof collectJournalUsage>>>()
    const summarized = deferred<Awaited<ReturnType<typeof fetchJournalSummary>>>()
    mockedCollect.mockReturnValueOnce(collected.promise)
    mockedFetch.mockReturnValueOnce(summarized.promise)
    const wrapper = mount(MemosView)
    await flushPromises()
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('dblclick')
    await wrapper.get('.journal-fetch-button').trigger('click')

    await wrapper.get('.journal-back-button').trigger('click')
    await flushPromises()
    await wrapper.get('[data-calendar-date="2026-08-14"]').trigger('dblclick')
    expect(wrapper.get('.journal-summary-panel').text()).toContain('尚未获取')
    expect(wrapper.get('.journal-summary-panel').text()).toContain('2026-08-13 的总结正在后台获取')
    expect(wrapper.get('.journal-fetch-button').attributes()).toHaveProperty('disabled')

    collected.resolve({ kind: 'collection', collection: collectedJournal() })
    await flushPromises()
    summarized.resolve({ kind: 'summary', summary: fetchedSummary() })
    await flushPromises()
    expect(wrapper.get('.journal-summary-panel').text()).toContain('尚未获取')
    expect(wrapper.get('.journal-summary-panel').text()).not.toContain('1 个工作项')

    await wrapper.get('.journal-back-button').trigger('click')
    await flushPromises()
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('dblclick')
    expect(wrapper.get('.journal-summary-panel').text()).toContain('1 个工作项 · 1 条知识')
    wrapper.unmount()
  })

  it('requires confirmation before invoking the knowledge capture command', async () => {
    const summary = fetchedSummary()
    summary.knowledgeItems.push({
      topic: '知识入库状态',
      summary: '同一来源会话的多条知识共享入库状态。',
      sourceSessionIds: ['session-1'],
    }, {
      topic: '跨会话候选一',
      summary: '另一条可入库知识。',
      sourceSessionIds: ['session-2'],
    }, {
      topic: '跨会话候选二',
      summary: '同样来自第二个可入库会话。',
      sourceSessionIds: ['session-2'],
    })
    summary.knowledgeCandidates.push({
      sourceSessionId: 'session-2',
      recommended: true,
      reason: 'long_knowledge_session',
      recommendedSkill: 'capture-conversations-to-vault',
    })
    mockedFetch.mockResolvedValue({ kind: 'summary', summary })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)
    const wrapper = mount(MemosView)
    await flushPromises()
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('dblclick')
    await wrapper.get('.journal-fetch-button').trigger('click')
    await flushPromises()
    const candidateCards = wrapper.findAll('.journal-knowledge-item')
    expect(candidateCards).toHaveLength(4)
    expect(wrapper.get('.journal-section-heading > span').text()).toBe('4 个候选')
    expect(candidateCards.every((card) => card.find('.journal-knowledge-capture-button').exists())).toBe(true)
    await candidateCards[0].get('.journal-knowledge-capture-button').trigger('click')
    await flushPromises()

    expect(confirm).toHaveBeenCalledWith('将调用“对话知识入库”Skill 写入本地知识库，是否继续？')
    expect(mockedCapture).toHaveBeenCalledWith(
      expect.objectContaining({ localDate: '2026-08-13' }),
      'session-1',
      true,
    )
    expect(wrapper.findAll('.journal-knowledge-capture-button').map((button) => button.text())).toEqual([
      '已入库',
      '已入库',
      '确认入库',
      '确认入库',
    ])
    expect(wrapper.find('.journal-knowledge-candidate').exists()).toBe(false)
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('keeps the knowledge section visible and explains why each item cannot be stored', async () => {
    const summary = fetchedSummary()
    summary.knowledgeItems = [{
      topic: '短会话知识',
      summary: '已提炼为日志知识，但来源会话较短。',
      sourceSessionIds: ['session-1'],
    }, {
      topic: '临时状态知识',
      summary: '会话长度足够，但主要是临时状态。',
      sourceSessionIds: ['session-2'],
    }]
    summary.knowledgeCandidates = [{
      sourceSessionId: 'session-2',
      recommended: false,
      reason: '内容以临时状态更新为主',
      recommendedSkill: 'capture-conversations-to-vault',
    }]
    const collection = collectedJournal()
    collection.sessions = [{
      ...collection.sessions[0],
      eligibility: {
        ...collection.sessions[0].eligibility,
        substantiveMessages: 8,
        contentChars: 4000,
        lengthClass: 'normal',
      },
    }, {
      ...collection.sessions[0],
      sessionId: 'session-2',
      title: '临时状态讨论',
      eligibility: {
        ...collection.sessions[0].eligibility,
        substantiveMessages: 28,
        contentChars: 14000,
        lengthClass: 'long',
      },
    }]
    mockedFetch.mockResolvedValue({ kind: 'summary', summary })
    mockedCollect.mockResolvedValue({ kind: 'collection', collection })

    const wrapper = mount(MemosView)
    await flushPromises()
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('dblclick')
    await wrapper.get('.journal-fetch-button').trigger('click')
    await flushPromises()

    expect(wrapper.get('#journal-knowledge-heading').text()).toBe('知识入库候选')
    expect(wrapper.findAll('.journal-knowledge-item')).toHaveLength(2)
    expect(wrapper.findAll('.journal-knowledge-item')[0].text()).toContain('暂不可入库')
    expect(wrapper.findAll('.journal-knowledge-item')[0].text()).toContain('至少 24 条有效消息或 12,000 字符')
    expect(wrapper.findAll('.journal-knowledge-item')[1].text()).toContain('总结器未推荐入库：内容以临时状态更新为主')
    expect(wrapper.get('.journal-knowledge-empty').text()).toContain('暂无可入库候选')
    expect(wrapper.find('.journal-knowledge-candidate').exists()).toBe(false)
    wrapper.unmount()
  })

  it('keeps collected AI usage visible when summary generation fails', async () => {
    mockedFetch.mockResolvedValue({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'journal_ai_failed',
        reason: 'journal_ai_failed',
        retryable: true,
      },
    })
    const wrapper = mount(MemosView)
    await flushPromises()
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('dblclick')
    await wrapper.get('.journal-fetch-button').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('AI 总结失败，用量数据已保留')
    expect(wrapper.text()).toContain('120')
    expect(wrapper.text()).toContain('完成日志编辑页')
    wrapper.unmount()
  })

  it('sorts the list by update time and keeps status mutation available', async () => {
    const older = summary({ updatedAtMs: 1_786_154_300_000 })
    const newer = summary({
      id: '019fe096-aeac-7bc1-8077-6e960dbc5571',
      title: '较新记录',
      updatedAtMs: 1_786_154_500_000,
    })
    mockedList
      .mockResolvedValueOnce({ kind: 'page', page: page([]) })
      .mockResolvedValueOnce({ kind: 'page', page: page([older, newer]) })
      .mockResolvedValueOnce({ kind: 'page', page: page([{ ...newer, status: 'completed', revision: 3 }, older]) })
    mockedGet.mockImplementation(async (id) => {
      const note = id === newer.id ? newer : older
      return { kind: 'document', document: document(note, note.title) }
    })
    const wrapper = mount(MemosView)
    await flushPromises()

    await wrapper.get('.notes-view-switch button:last-child').trigger('click')
    await flushPromises()
    const rows = wrapper.findAll('.notes-memo-list tbody tr')
    expect(rows[0].text()).toContain('较新记录')
    await rows[0].get('.notes-complete-toggle').trigger('click')
    await flushPromises()
    expect(mockedWrite).toHaveBeenCalledWith(expect.objectContaining({
      kind: 'save',
      id: newer.id,
      meta: expect.objectContaining({ status: 'completed' }),
    }))
    wrapper.unmount()
  })

  it('keeps typed bridge failure visible and retries the current calendar', async () => {
    mockedList
      .mockResolvedValueOnce({
        kind: 'error',
        error: {
          kind: 'transport',
          code: 'desktop_bridge_unavailable',
          reason: 'desktop_bridge_unavailable',
          retryable: true,
        },
      })
      .mockResolvedValueOnce({ kind: 'page', page: page([]) })
    const wrapper = mount(MemosView)
    await flushPromises()

    expect(wrapper.text()).toContain('日志日历不可用')
    expect(wrapper.text()).toContain('desktop_bridge_unavailable')
    await wrapper.get('.notes-calendar-message button').trigger('click')
    await flushPromises()
    expect(mockedList).toHaveBeenCalledTimes(2)
    wrapper.unmount()
  })
})
