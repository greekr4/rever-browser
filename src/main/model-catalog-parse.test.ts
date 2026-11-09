import { describe, expect, it } from 'vitest'

import { claudeModelsFromInit, codexModelsFromList, parseCatalogFile } from './model-catalog-parse'

describe('claudeModelsFromInit', () => {
  it('initialize 응답의 모델을 이름·설명·추론 수준·빠른 모드와 함께 옮긴다', () => {
    const init = {
      models: [
        { value: 'default', displayName: 'Default (recommended)', description: 'Fable 5.1', supportedEffortLevels: ['low', 'high'] },
        { value: 'opus', displayName: 'Opus 5.5', supportsFastMode: true },
        { displayName: 'no value — dropped' }
      ]
    }
    expect(claudeModelsFromInit(init)).toEqual([
      { modelId: 'default', name: 'Default (recommended)', description: 'Fable 5.1', effortLevels: ['low', 'high'], supportsFastMode: false },
      { modelId: 'opus', name: 'Opus 5.5', description: null, effortLevels: [], supportsFastMode: true }
    ])
  })

  it('모델 목록이 없으면 빈 배열', () => {
    expect(claudeModelsFromInit({})).toEqual([])
  })
})

describe('codexModelsFromList', () => {
  it('숨김 모델을 빼고 기본 모델을 맨 앞에 둔다', () => {
    const res = {
      data: [
        { id: 'gpt-6-sol', displayName: 'GPT-6-Sol', description: 'b', hidden: false, isDefault: false },
        { id: 'gpt-old', displayName: 'Old', hidden: true },
        { id: 'gpt-6.1-sol', displayName: 'GPT-6.1-Sol', description: 'a', hidden: false, isDefault: true }
      ],
      nextCursor: null
    }
    expect(codexModelsFromList(res).map((m) => m.modelId)).toEqual(['gpt-6.1-sol', 'gpt-6-sol'])
    expect(codexModelsFromList(res)[0]).toEqual({ modelId: 'gpt-6.1-sol', name: 'GPT-6.1-Sol', description: 'a' })
  })
})

describe('parseCatalogFile', () => {
  it('알아보는 형식만 받아들이고 나머지는 빈 캐시로 둔다', () => {
    const ok = { version: 1, agents: { codex: { models: [{ modelId: 'x', name: 'X' }], fetchedAt: '2026-11-06T00:00:00Z' }, bad: { models: 'nope' } } }
    expect(Object.keys(parseCatalogFile(JSON.stringify(ok)).agents)).toEqual(['codex'])
    expect(parseCatalogFile('{"version":2}').agents).toEqual({})
    expect(parseCatalogFile('not json').agents).toEqual({})
  })
})
