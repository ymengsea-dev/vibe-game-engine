(function (root) {
  'use strict';

  function clampExplorerWidth(width, availableWidth) {
    var room = Math.max(0, availableWidth - 260);
    return Math.min(Math.max(180, width), 500, room);
  }

  function installExplorerSplitter(handle) {
    var width = 205;
    var startX = 0;
    var startWidth = 0;

    function setWidth(next) {
      width = clampExplorerWidth(next, root.innerWidth);
      root.document.documentElement.style.setProperty('--explorer-width', width + 'px');
      if (root.studio && root.studio.relayout) root.studio.relayout();
    }

    handle.addEventListener('pointerdown', function (event) {
      if (event.button !== 0) return;
      startX = event.clientX;
      startWidth = width;
      handle.setPointerCapture(event.pointerId);
      event.preventDefault();
    });
    handle.addEventListener('pointermove', function (event) {
      if (handle.hasPointerCapture(event.pointerId)) {
        setWidth(startWidth + event.clientX - startX);
      }
    });
    handle.addEventListener('keydown', function (event) {
      if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
      setWidth(width + (event.key === 'ArrowLeft' ? -10 : 10));
      event.preventDefault();
    });
    root.addEventListener('resize', function () { setWidth(width); });
    setWidth(width);
  }

  var api = { clampExplorerWidth: clampExplorerWidth, installExplorerSplitter: installExplorerSplitter };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  if (root) root.StudioLayout = api;
})(typeof window === 'undefined' ? globalThis : window);
