// A network flavor cannot expose process/filesystem host tools. No ambient context.
const denied = () => { throw new Error('capability-denied: host-tools and preopens are unavailable to this network flavor') }
const service = () => new Proxy(Object.create(null), {get:(_target,key)=>key==='then'?undefined:denied})
export class HostServicesSeam {
  clat=service(); fs=service(); shell=service(); sessions=service(); agents=Object.freeze({currentInitiator:()=>undefined,get:()=>undefined,list:()=>[],roots:()=>[],create:denied,resume:denied,setFactory:denied})
  attachContext(_context) {}
  updateContext(_context) { denied() }
}
