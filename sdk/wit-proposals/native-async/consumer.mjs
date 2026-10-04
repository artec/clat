import { dnsResolve, httpRequest } from 'clat:net-probe/egress@0.1.0'
export async function run() {
  const addresses = await dnsResolve('https://api.deepseek.com')
  const response = await httpRequest('https://api.deepseek.com/')
  return JSON.stringify({ addresses, response })
}
