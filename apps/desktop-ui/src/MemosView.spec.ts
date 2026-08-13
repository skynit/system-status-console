import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { deleteNote, getBackendHealth, getNote, listNotes, writeNote } from './backend'
import MemosView from './views/MemosView.vue'
import type { NoteDocument, NoteSummary } from './types'

vi.mock('./backend', () => ({
  getBackendHealth: vi.fn(),
  getNote: vi.fn(),
  listNotes: vi.fn(),
  deleteNote: vi.fn(),
  writeNote: vi.fn(),
}))

const mockedHealth = vi.mocked(getBackendHealth)
const mockedGet = vi.mocked(getNote)
const mockedList = vi.mocked(listNotes)
const mockedDelete = vi.mocked(deleteNote)
const mockedWrite = vi.mocked(writeNote)

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

describe('MemosView calendar and list', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(2026, 7, 12, 10, 0, 0))
    mockedHealth.mockReset()
    mockedGet.mockReset()
    mockedList.mockReset()
    mockedDelete.mockReset()
    mockedWrite.mockReset()
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
  })

  it('renders the six-week calendar without the removed monthly empty message', async () => {
    const wrapper = mount(MemosView)
    await flushPromises()

    expect(wrapper.text()).toContain('2026年8月')
    expect(wrapper.text()).not.toContain('本月暂无备忘录')
    expect(wrapper.findAll('.notes-calendar-week')).toHaveLength(6)
    expect(wrapper.findAll('.notes-calendar-day')).toHaveLength(42)
    expect(wrapper.get('[data-calendar-date="2026-08-12"]').classes()).toContain('is-today')
    expect(wrapper.get('.notes-view-switch button[aria-pressed="true"]').text()).toContain('日历')
    expect(mockedList).toHaveBeenCalledWith(expect.objectContaining({
      diaryDateFrom: '2026-08-01',
      diaryDateTo: '2026-08-31',
      sort: 'diary_date_desc',
    }))
    wrapper.unmount()
  })

  it('renders each real body preview and distinguishes completed notes without counts', async () => {
    const active = summary()
    const completed = summary({
      id: '019fe096-aeac-7bc1-8077-6e960dbc5571',
      status: 'completed',
    })
    mockedList.mockResolvedValue({ kind: 'page', page: page([active, completed]) })
    mockedGet.mockImplementation(async (id) => ({
      kind: 'document',
      document: id === active.id
        ? document(active, '检查发布包签名')
        : document(completed, '更新交付记录'),
    }))
    const wrapper = mount(MemosView)
    await flushPromises()

    const date = wrapper.get('[data-calendar-date="2026-08-11"]')
    expect(date.text()).toContain('检查发布包签名')
    expect(date.text()).toContain('更新交付记录')
    expect(date.text()).not.toContain('2 条备忘录')
    expect(date.findAll('.notes-calendar-memo')).toHaveLength(2)
    expect(date.findAll('.notes-calendar-memo')[1].classes()).toContain('is-completed')
    wrapper.unmount()
  })

  it('opens the real note document on one click and opens creation on a date double-click', async () => {
    const note = summary()
    mockedList.mockResolvedValue({ kind: 'page', page: page([note]) })
    mockedGet.mockResolvedValue({ kind: 'document', document: document(note, '完整正文内容') })
    const wrapper = mount(MemosView)
    await flushPromises()

    await wrapper.get('.notes-calendar-memo').trigger('click')
    await flushPromises()
    expect(wrapper.get('.notes-memo-editor').attributes('aria-label')).toBe('查看备忘录')
    expect(wrapper.get('.notes-memo-editor').attributes('role')).toBe('dialog')
    expect(wrapper.get('.notes-memo-editor').attributes('aria-modal')).toBe('true')
    expect(wrapper.get('.notes-document-view').text()).toContain('完整正文内容')

    await wrapper.get('button[aria-label="关闭备忘录弹窗"]').trigger('click')
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('dblclick')
    expect(wrapper.get('.notes-memo-editor').attributes('aria-label')).toBe('添加备忘录')
    expect(wrapper.get('.notes-memo-editor').text()).toContain('2026-08-13')
    wrapper.unmount()
  })

  it('marks a note completed and deletes it from the detail dialog', async () => {
    const note = summary()
    const completed = { ...note, status: 'completed' as const, revision: 3 }
    mockedList
      .mockResolvedValueOnce({ kind: 'page', page: page([note]) })
      .mockResolvedValueOnce({ kind: 'page', page: page([completed]) })
      .mockResolvedValueOnce({ kind: 'page', page: page([]) })
    mockedGet.mockResolvedValue({ kind: 'document', document: document(note, '完整正文内容') })
    mockedWrite.mockResolvedValue({
      kind: 'mutation',
      result: { kind: 'stored', note: completed },
    })
    mockedDelete.mockResolvedValue({
      kind: 'mutation',
      result: { kind: 'deleted', note: { ...completed, revision: 4, deletedAtMs: 1_786_154_500_000 } },
    })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)
    const wrapper = mount(MemosView)
    await flushPromises()

    await wrapper.get('.notes-calendar-memo').trigger('click')
    await flushPromises()
    await wrapper.get('.notes-document-status').trigger('click')
    await flushPromises()
    expect(mockedWrite).toHaveBeenCalledWith(expect.objectContaining({
      kind: 'save',
      id: note.id,
      expectedRevision: note.revision,
      meta: expect.objectContaining({ status: 'completed' }),
    }))
    expect(wrapper.get('.notes-document-status').attributes('aria-pressed')).toBe('true')
    expect(wrapper.get('.notes-document-status').text()).toContain('已完成')

    await wrapper.get('.notes-document-delete').trigger('click')
    await flushPromises()
    expect(confirm).toHaveBeenCalledWith('确定删除“发布检查”吗？')
    expect(mockedDelete).toHaveBeenCalledWith(note.id, completed.revision)
    expect(wrapper.find('.notes-memo-editor').exists()).toBe(false)
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('keeps the note open when detail deletion is not confirmed', async () => {
    const note = summary()
    mockedList.mockResolvedValue({ kind: 'page', page: page([note]) })
    mockedGet.mockResolvedValue({ kind: 'document', document: document(note) })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false)
    const wrapper = mount(MemosView)
    await flushPromises()

    await wrapper.get('.notes-calendar-memo').trigger('click')
    await flushPromises()
    await wrapper.get('.notes-document-delete').trigger('click')
    await flushPromises()

    expect(mockedDelete).not.toHaveBeenCalled()
    expect(wrapper.find('.notes-memo-editor').exists()).toBe(true)
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('creates a note for the double-clicked date through the existing write contract', async () => {
    const wrapper = mount(MemosView)
    await flushPromises()

    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('dblclick')
    await wrapper.get('#new-note-title').setValue('新的备忘录')
    await wrapper.get('.notes-create-body textarea').setValue('正文')
    await wrapper.get('.notes-create-form').trigger('submit')
    await flushPromises()

    expect(mockedWrite).toHaveBeenCalledWith({
      kind: 'create',
      meta: {
        title: '新的备忘录',
        diaryDate: '2026-08-13',
        tags: [],
        status: 'active',
        pinned: false,
      },
      bodyMarkdown: '正文',
    })
    expect(wrapper.find('.notes-memo-editor').exists()).toBe(false)
    wrapper.unmount()
  })

  it('switches to the all-notes list, sorts rows by time, and persists completion', async () => {
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
    expect(mockedList).toHaveBeenLastCalledWith(expect.objectContaining({
      diaryDateFrom: null,
      diaryDateTo: null,
      sort: 'updated_desc',
    }))
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

  it('keeps roving keyboard focus and allows Enter to add on the selected date', async () => {
    const wrapper = mount(MemosView, { attachTo: document.body })
    await flushPromises()
    const today = wrapper.get('[data-calendar-date="2026-08-12"]')
    ;(today.element as HTMLElement).focus()

    await today.trigger('keydown', { key: 'ArrowRight' })
    await flushPromises()
    expect(wrapper.get('[data-calendar-date="2026-08-13"]').attributes('tabindex')).toBe('0')
    await wrapper.get('[data-calendar-date="2026-08-13"]').trigger('keydown', { key: 'Enter' })
    expect(wrapper.get('.notes-memo-editor').attributes('aria-label')).toBe('添加备忘录')
    wrapper.unmount()
  })

  it('keeps typed bridge failure visible and retries the current view', async () => {
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

    expect(wrapper.text()).toContain('备忘录日历不可用')
    expect(wrapper.text()).toContain('desktop_bridge_unavailable')
    await wrapper.get('.notes-calendar-message button').trigger('click')
    await flushPromises()
    expect(mockedList).toHaveBeenCalledTimes(2)
    expect(wrapper.text()).not.toContain('本月暂无备忘录')
    wrapper.unmount()
  })
})
