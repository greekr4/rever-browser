import { expect, test } from 'bun:test'

import { applyMessage } from './world'

const player = () => ({ id: 'p1', name: 'player-1', x: 50, y: 50 })

test('정상 이동은 확정되어 모두에게 전파된다', () => {
  const p = player()
  const r = applyMessage(p, '{"type":"move","x":55,"y":58}')
  expect(r.toAll).toEqual([{ type: 'pos', id: 'p1', name: 'player-1', x: 55, y: 58 }])
  expect(p).toMatchObject({ x: 55, y: 58 })
})

test('맵 밖·너무 먼 이동은 거부되고 원래 위치로 되돌린다', () => {
  const p = player()
  expect(applyMessage(p, '{"type":"move","x":30,"y":11111}').toSender[0]).toMatchObject({ code: 'out_of_bounds' })
  expect(applyMessage(p, '{"type":"move","x":90,"y":90}').toSender[0]).toMatchObject({ code: 'too_fast' })
  expect(p).toMatchObject({ x: 50, y: 50 })
})

test('숨은 teleport는 아무 검사 없이 받아 준다', () => {
  const p = player()
  const r = applyMessage(p, '{"type":"teleport","x":95,"y":-40}')
  expect(r.toAll[0]).toMatchObject({ via: 'teleport', x: 95, y: -40 })
})

test('서버 전용 메시지·알 수 없는 종류·깨진 JSON은 거부한다', () => {
  const p = player()
  expect(applyMessage(p, '{"type":"echo","received":"x"}').toSender[0]).toMatchObject({ code: 'client_cannot_send' })
  expect(applyMessage(p, '{"type":"dance"}').toSender[0]).toMatchObject({ code: 'unknown_type' })
  expect(applyMessage(p, 'not json').toSender[0]).toMatchObject({ code: 'bad_json' })
  expect(applyMessage(p, '{"type":"chat","text":"hi"}').toAll[0]).toMatchObject({ type: 'chat', text: 'hi' })
})
