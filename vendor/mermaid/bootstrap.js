// accent's own, not mermaid's: the bootstrap that draws a note's diagrams, which the desktop
// preview (`apps/gtk/src/preview.rs`) and the Android view (`NoteScreen.kt`) both run, so the two
// cannot drift. Each caller picks the theme by its page, and the desktop passes `keep` to move its
// scroll-sync marker out of a fence before the fence goes.
//
// Each fence becomes a `<pre class="mermaid">` from the code element's `textContent`, which undoes
// pulldown-cmark's HTML escaping and hands mermaid the source exactly as the author typed it; a
// fence mermaid cannot draw gets that source back, the contract the math fallback has, so a typo
// never blanks a block. Answers the promise of the drawing, or nothing on a page without a fence.
function accentDiagrams(theme, keep) {
  var blocks = document.querySelectorAll('pre > code.language-mermaid');
  if (!blocks.length) { return; }
  var nodes = [];
  for (var i = 0; i < blocks.length; i++) {
    var fence = blocks[i].parentElement;
    var pre = document.createElement('pre');
    pre.className = 'mermaid';
    pre.textContent = blocks[i].textContent;
    if (keep) { keep(fence); }
    fence.parentElement.replaceChild(pre, fence);
    nodes.push(pre);
  }
  var sources = nodes.map(function (n) { return n.textContent; });
  mermaid.initialize({ startOnLoad: false, theme: theme, suppressErrorRendering: true });
  return mermaid.run({ nodes: nodes }).catch(function () {}).then(function () {
    // suppressErrorRendering empties a fence it cannot parse rather than drawing an error graphic,
    // so its source goes back in and a broken diagram stays readable, as a rejected formula does.
    for (var j = 0; j < nodes.length; j++) {
      if (!nodes[j].querySelector('svg')) { nodes[j].textContent = sources[j]; }
    }
  });
}
