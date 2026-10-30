/* Build-time adaptation of the pinned @xterm/addon-canvas 0.7.0 bundle.
 * WKWebView can ignore user fonts on detached raster canvases (WebKit 316777).
 * Connect only the private glyph scratch canvas before its context is created;
 * atlas pages and visible terminal layers keep their upstream lifecycle.
 */
import {readFileSync} from 'node:fs';
import type {BunPlugin} from 'bun';

function replaceOnce(source:string,needle:string,replacement:string):string{
  if(source.split(needle).length!==2)throw new Error('Canvas font compatibility patch no longer matches the pinned addon');
  return source.replace(needle,replacement);
}

export function connectedCanvasFonts():BunPlugin{
  return {name:'connected-canvas-fonts',setup(build){
    build.onLoad({filter:/[/\\]@xterm[/\\]addon-canvas[/\\]lib[/\\]addon-canvas\.js$/},args=>{
      let source=readFileSync(args.path,'utf8');
      source=replaceOnce(source,'this._tmpCtx=(0,o.throwIfFalsy)(this._tmpCanvas.getContext',
        'this._tmpCanvas.hidden=true,this._document.body.appendChild(this._tmpCanvas),this._tmpCtx=(0,o.throwIfFalsy)(this._tmpCanvas.getContext');
      source=replaceOnce(source,'dispose(){for(const e of this.pages)e.canvas.remove();',
        'dispose(){this._tmpCanvas.remove();for(const e of this.pages)e.canvas.remove();');
      return {contents:source,loader:'js'};
    });
  }};
}
