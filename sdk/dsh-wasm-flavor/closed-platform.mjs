// Permanent denies for facilities outside the network flavor. No Node platform emulation.
export const deny=()=>{throw new Error('capability-denied: process and filesystem facilities are unavailable')}
export const accessSync=deny,realpathSync=deny,statSync=deny,tmpdir=deny
export const constants=new Proxy(Object.create(null),{get:deny})
