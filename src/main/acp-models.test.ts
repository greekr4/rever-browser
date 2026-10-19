import { describe, expect, it } from 'vitest'

import { modelStateFromSession } from './acp-models'

describe('modelStateFromSession', () => {
  it('구버전 에이전트의 models 필드를 그대로 쓴다', () => {
    const state = modelStateFromSession({
      models: {
        availableModels: [{ modelId: 'opus', name: 'Opus' }],
        currentModelId: 'opus'
      }
    })
    expect(state).toEqual({
      availableModels: [{ modelId: 'opus', name: 'Opus' }],
      currentModelId: 'opus',
      modelConfigId: null
    })
  })

  it('신버전 에이전트는 category가 model인 config option에서 모델 목록을 만든다', () => {
    const state = modelStateFromSession({
      configOptions: [
        { id: 'mode', name: 'Mode', category: 'mode', type: 'select', currentValue: 'default', options: [] },
        {
          id: 'model',
          name: 'Model',
          category: 'model',
          type: 'select',
          currentValue: 'opus',
          options: [
            { value: 'default', name: 'Default', description: 'Recommended' },
            { value: 'opus', name: 'Opus' }
          ]
        }
      ]
    })
    expect(state).toEqual({
      availableModels: [
        { modelId: 'default', name: 'Default', description: 'Recommended' },
        { modelId: 'opus', name: 'Opus', description: null }
      ],
      currentModelId: 'opus',
      modelConfigId: 'model'
    })
  })

  it('그룹으로 묶인 옵션은 펼친다', () => {
    const state = modelStateFromSession({
      configOptions: [
        {
          id: 'model',
          name: 'Model',
          category: 'model',
          type: 'select',
          currentValue: 'b',
          options: [
            { group: 'g1', name: 'G1', options: [{ value: 'a', name: 'A' }] },
            { group: 'g2', name: 'G2', options: [{ value: 'b', name: 'B' }] }
          ]
        }
      ]
    })
    expect(state.availableModels.map((m) => m.modelId)).toEqual(['a', 'b'])
    expect(state.currentModelId).toBe('b')
  })

  it('모델 정보가 없으면 빈 상태를 돌려준다', () => {
    expect(modelStateFromSession({})).toEqual({
      availableModels: [],
      currentModelId: null,
      modelConfigId: null
    })
  })
})
