// Explicit manual capture only; ordinary CI continues to use playwright.config.js.
const base = require('./playwright.config');
module.exports = {...base, testDir:'./manual', use:{...base.use, reducedMotion:'reduce'}};
