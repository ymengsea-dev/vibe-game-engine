const assert = require('node:assert/strict');
const test = require('node:test');
const { clampExplorerWidth, installExplorerSplitter } = require('./layout.js');

test('explorer starts at the approved prototype width', () => {
  let width;
  globalThis.innerWidth = 1000;
  globalThis.document = { documentElement: { style: { setProperty(_name, value) { width = value; } } } };
  globalThis.addEventListener = () => {};
  installExplorerSplitter({ addEventListener() {} });
  assert.equal(width, '205px');
});

test('explorer width stays within its limits and leaves room for code', () => {
  assert.equal(clampExplorerWidth(90, 1000), 180);
  assert.equal(clampExplorerWidth(900, 1000), 500);
  assert.equal(clampExplorerWidth(300, 500), 240);
  assert.equal(clampExplorerWidth(300, 400), 140);
});
