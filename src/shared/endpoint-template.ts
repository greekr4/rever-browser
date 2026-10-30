/** /users/12/orders/9f8a-... → /users/:id/orders/:id */
export function templatePath(path: string): string {
  return path
    .split('/')
    .map((seg) => {
      if (!seg) return seg
      if (/^\d+$/.test(seg)) return ':id'
      if (/^[0-9a-f]{16,}$/i.test(seg)) return ':id'
      if (/^[0-9a-f]{8}-[0-9a-f]{4}-/i.test(seg)) return ':id'
      return seg
    })
    .join('/')
}
