export interface TrafficEntry {
  requestId: string
  url: string
  method: string
  resourceType: string
  status?: number
  mimeType?: string
  encodedDataLength?: number
  startedAt: number
  completedAt?: number
}

export type NetworkEvent =
  | {
      type: 'request'
      request_id: string
      url: string
      method: string
      resource_type: string
      timestamp: number
    }
  | {
      type: 'response'
      request_id: string
      status: number
      mime_type: string
      timestamp: number
    }
  | {
      type: 'finished'
      request_id: string
      encoded_data_length: number
      timestamp: number
    }
