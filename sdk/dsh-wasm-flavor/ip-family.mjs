import ipaddr from '../dsh-adapter/examples/official-web/node_modules/ipaddr.js/lib/ipaddr.js'
export function isIP(value) {
  if(typeof value!=='string'||value.includes('%'))return 0
  try {const parsed=ipaddr.parse(value);return parsed.kind()==='ipv6'&&value.includes(':')?6:parsed.kind()==='ipv4'&&parsed.toString()===value?4:0}catch{return 0}
}
