const __vite__mapDeps=(i,m=__vite__mapDeps,d=(m.f||(m.f=["assets/aiua-app-BGh3FSa8.js","assets/macos-app-XQ_ycgsz.js","assets/finder-app-CdFc47b7.js","assets/notes-app-DrPuRKd0.js"])))=>i.map(i=>d[i]);
(function(){const e=document.createElement("link").relList;if(e&&e.supports&&e.supports("modulepreload"))return;for(const n of document.querySelectorAll('link[rel="modulepreload"]'))i(n);new MutationObserver(n=>{for(const o of n)if(o.type==="childList")for(const a of o.addedNodes)a.tagName==="LINK"&&a.rel==="modulepreload"&&i(a)}).observe(document,{childList:!0,subtree:!0});function t(n){const o={};return n.integrity&&(o.integrity=n.integrity),n.referrerPolicy&&(o.referrerPolicy=n.referrerPolicy),n.crossOrigin==="use-credentials"?o.credentials="include":n.crossOrigin==="anonymous"?o.credentials="omit":o.credentials="same-origin",o}function i(n){if(n.ep)return;n.ep=!0;const o=t(n);fetch(n.href,o)}})();const Q="modulepreload",K=function(r){return"/"+r},j={},w=function(e,t,i){let n=Promise.resolve();if(t&&t.length>0){let l=function(c){return Promise.all(c.map(p=>Promise.resolve(p).then(u=>({status:"fulfilled",value:u}),u=>({status:"rejected",reason:u}))))};document.getElementsByTagName("link");const a=document.querySelector("meta[property=csp-nonce]"),d=a?.nonce||a?.getAttribute("nonce");n=l(t.map(c=>{if(c=K(c),c in j)return;j[c]=!0;const p=c.endsWith(".css"),u=p?'[rel="stylesheet"]':"";if(document.querySelector(`link[href="${c}"]${u}`))return;const h=document.createElement("link");if(h.rel=p?"stylesheet":Q,p||(h.as="script"),h.crossOrigin="",h.href=c,d&&h.setAttribute("nonce",d),document.head.appendChild(h),p)return new Promise((v,_)=>{h.addEventListener("load",v),h.addEventListener("error",()=>_(new Error(`Unable to preload CSS for ${c}`)))})}))}function o(a){const d=new Event("vite:preloadError",{cancelable:!0});if(d.payload=a,window.dispatchEvent(d),!d.defaultPrevented)throw a}return n.then(a=>{for(const d of a||[])d.status==="rejected"&&o(d.reason);return e().catch(o)})};class ee extends HTMLElement{static get observedAttributes(){return["background-image","background-color"]}constructor(){super(),this.attachShadow({mode:"open"}),this._activeWindow=null,this._updatePromise=null,this._resolveUpdate=null}connectedCallback(){this._render(),this._setupEventListeners()}disconnectedCallback(){this._cleanup()}attributeChangedCallback(e,t,i){t!==i&&this._handleAttributeChange(e,i)}get updateComplete(){return this._updatePromise||(this._updatePromise=new Promise(e=>{this._resolveUpdate=e}),Promise.resolve().then(()=>{this._resolveUpdate&&(this._resolveUpdate(),this._updatePromise=null,this._resolveUpdate=null)})),this._updatePromise}setBackground(e,t){e?this.setAttribute("background-image",e):this.removeAttribute("background-image"),t&&this.setAttribute("background-color",t)}getActiveWindow(){return this._activeWindow}setActiveWindow(e){this._activeWindow=e}clearActiveWindow(){this._activeWindow=null}_render(){const e=document.createElement("template");e.innerHTML=`
      <style>
        :host {
          display: block;
          width: 100vw;
          height: calc(100vh - 28px);
          position: fixed;
          top: 28px;
          left: 0;
          overflow: hidden;
          background-color: var(--system-background, #1e1e1e);
          background-size: cover;
          background-position: center;
          background-repeat: no-repeat;
        }

        /* Gradient orbs for macOS-style background */
        .gradient-layer {
          position: absolute;
          top: 0;
          left: 0;
          right: 0;
          bottom: 0;
          overflow: hidden;
          z-index: 0;
          pointer-events: none;
        }

        .gradient-orb {
          position: absolute;
          border-radius: 50%;
          filter: blur(80px);
          opacity: 0.6;
        }

        .orb-1 {
          width: 600px;
          height: 600px;
          top: -200px;
          left: -200px;
          background: radial-gradient(circle, #93c5fd 0%, transparent 70%);
        }

        .orb-2 {
          width: 500px;
          height: 500px;
          top: -150px;
          right: -100px;
          background: radial-gradient(circle, #c4b5fd 0%, transparent 70%);
        }

        .orb-3 {
          width: 550px;
          height: 550px;
          bottom: -150px;
          left: 20%;
          background: radial-gradient(circle, #f472b6 0%, transparent 70%);
        }

        .orb-4 {
          width: 450px;
          height: 450px;
          bottom: -100px;
          right: 10%;
          background: radial-gradient(circle, #fb923c 0%, transparent 70%);
        }

        .orb-5 {
          width: 400px;
          height: 400px;
          bottom: 0;
          left: 0;
          background: radial-gradient(circle, #fcd34d 0%, transparent 70%);
        }

        .desktop-surface {
          width: 100%;
          height: 100%;
          position: relative;
          z-index: 1;
        }

        /* Accessibility: Focus visible styles */
        :host(:focus-visible) {
          outline: 2px solid var(--accent-color);
          outline-offset: -2px;
        }
      </style>

      <div class="gradient-layer">
        <div class="gradient-orb orb-1"></div>
        <div class="gradient-orb orb-2"></div>
        <div class="gradient-orb orb-3"></div>
        <div class="gradient-orb orb-4"></div>
        <div class="gradient-orb orb-5"></div>
      </div>

      <div class="desktop-surface" role="main" aria-label="Desktop" tabindex="-1">
        <!-- Windows and dock will be slotted here -->
        <slot></slot>
      </div>
    `,this.shadowRoot.appendChild(e.content.cloneNode(!0)),this._applyBackground()}_setupEventListeners(){const e=this.shadowRoot.querySelector(".desktop-surface");this._clickHandler=t=>{t.target===e&&this._handleDesktopClick(t)},e.addEventListener("click",this._clickHandler),this._contextMenuHandler=t=>{t.target===e&&this._handleContextMenu(t)},e.addEventListener("contextmenu",this._contextMenuHandler),this._windowActivateHandler=t=>{t.detail&&t.detail.window&&(this._activeWindow=t.detail.window)},this.addEventListener("window-activated",this._windowActivateHandler),this._windowFocusHandler=t=>{const i=t.target;i&&i.tagName==="MACOS-WINDOW"&&(this._activeWindow=i)},this.addEventListener("window-focus",this._windowFocusHandler)}_cleanup(){const e=this.shadowRoot.querySelector(".desktop-surface");e&&this._clickHandler&&e.removeEventListener("click",this._clickHandler),e&&this._contextMenuHandler&&e.removeEventListener("contextmenu",this._contextMenuHandler),this._windowActivateHandler&&this.removeEventListener("window-activated",this._windowActivateHandler),this._windowFocusHandler&&this.removeEventListener("window-focus",this._windowFocusHandler)}_handleAttributeChange(e,t){(e==="background-image"||e==="background-color")&&this._applyBackground()}_applyBackground(){const e=this.getAttribute("background-image"),t=this.getAttribute("background-color"),i=this.shadowRoot.querySelector(".gradient-layer");e?(this.style.backgroundImage=`url("${e}")`,i&&(i.style.display="none")):(this.style.backgroundImage="",i&&(i.style.display="")),t&&(this.style.backgroundColor=t)}_handleDesktopClick(e){this._activeWindow=null,this.dispatchEvent(new CustomEvent("desktop-click",{detail:{x:e.clientX,y:e.clientY},bubbles:!0,composed:!0}))}_handleContextMenu(e){e.preventDefault(),this.dispatchEvent(new CustomEvent("desktop-contextmenu",{detail:{x:e.clientX,y:e.clientY},bubbles:!0,composed:!0}))}}customElements.define("likes-desktop",ee);class te extends HTMLElement{static get observedAttributes(){return["title","x","y","width","height","min-width","min-height","z-index","state","focused"]}constructor(){super(),this.attachShadow({mode:"open"}),this._dragging=!1,this._resizing=!1,this._dragStart={x:0,y:0},this._resizeStart={x:0,y:0,width:0,height:0},this._resizeDirection=null,this._updatePromise=null,this._resolveUpdate=null}connectedCallback(){this.hasAttribute("state")||this.setAttribute("state","normal"),this._render(),this._setupEventListeners(),this._updatePosition(),this._updateSize()}disconnectedCallback(){this._cleanup()}attributeChangedCallback(e,t,i){t!==i&&this._handleAttributeChange(e,i)}get updateComplete(){return this._updatePromise||(this._updatePromise=new Promise(e=>{this._resolveUpdate=e}),Promise.resolve().then(()=>{this._resolveUpdate&&(this._resolveUpdate(),this._updatePromise=null,this._resolveUpdate=null)})),this._updatePromise}close(){this.dispatchEvent(new CustomEvent("window-close",{detail:{windowId:this.id},bubbles:!0,composed:!0})),this.remove()}minimize(){this.setAttribute("state","minimized"),this.dispatchEvent(new CustomEvent("window-minimize",{detail:{windowId:this.id},bubbles:!0}))}maximize(){const t=this.getAttribute("state")==="maximized"?"normal":"maximized";this.setAttribute("state",t),this.dispatchEvent(new CustomEvent("window-maximize",{detail:{windowId:this.id,state:t},bubbles:!0}))}toggleFullscreen(){const t=this.getAttribute("state")==="fullscreen"?"normal":"fullscreen";this.setAttribute("state",t);const i=document.getElementById("main-menubar");t==="fullscreen"?(i&&(i.style.transform="translateY(-100%)",i.style.transition="transform 0.3s ease-in-out",i.style.position="fixed",i.style.zIndex="10001"),this._setupFullscreenMenuReveal()):(i&&(i.style.transform="",i.style.transition="",i.style.position="",i.style.zIndex=""),this._cleanupFullscreenMenuReveal()),this.dispatchEvent(new CustomEvent("window-fullscreen",{detail:{windowId:this.id,state:t},bubbles:!0}))}_setupFullscreenMenuReveal(){this._fullscreenMouseHandler=e=>{const t=document.getElementById("main-menubar");if(t){if(window.desktopManager){const i=window.desktopManager.getCurrentIndex();if(window.desktopManager.getDesktopForWindow(this.id)!==i)return}e.clientY<=5?(t.style.transform="translateY(0)",this.setAttribute("data-reveal-ui","true")):e.clientY>100&&(t.style.transform="translateY(-100%)",this.removeAttribute("data-reveal-ui"))}},document.addEventListener("mousemove",this._fullscreenMouseHandler)}_cleanupFullscreenMenuReveal(){this._fullscreenMouseHandler&&(document.removeEventListener("mousemove",this._fullscreenMouseHandler),this._fullscreenMouseHandler=null),this.removeAttribute("data-reveal-ui")}focus(){this.setAttribute("focused","true"),this.dispatchEvent(new CustomEvent("window-focus",{detail:{windowId:this.id},bubbles:!0}))}setPosition(e,t){this.setAttribute("x",e.toString()),this.setAttribute("y",t.toString())}setSize(e,t){const i=parseInt(this.getAttribute("min-width")||"400",10),n=parseInt(this.getAttribute("min-height")||"300",10),o=Math.max(e,i),a=Math.max(t,n);this.setAttribute("width",o.toString()),this.setAttribute("height",a.toString())}_render(){const e=document.createElement("template");e.innerHTML=`
      <style>
        :host {
          display: block;
          position: absolute;
          background: var(--window-background);
          backdrop-filter: var(--window-blur);
          -webkit-backdrop-filter: var(--window-blur);
          border-radius: var(--radius-window);
          box-shadow: var(--shadow-window);
          overflow: hidden;
          animation: windowFadeIn var(--duration-medium) var(--easing-spring);
          /* T013: CSS containment for performance optimization */
          contain: layout style paint;
        }

        @keyframes windowFadeIn {
          from {
            opacity: 0;
            transform: scale(0.95);
          }
          to {
            opacity: 1;
            transform: scale(1);
          }
        }

        :host([state="minimized"]) {
          display: none;
        }

        :host([state="maximized"]) {
          width: 100vw !important;
          height: calc(100vh - 28px) !important;
          top: 28px !important;
          left: 0 !important;
          border-radius: 0;
          box-shadow: none;
        }

        :host([state="fullscreen"]) {
          position: fixed !important;
          width: 100vw !important;
          height: 100vh !important;
          top: 0 !important;
          left: 0 !important;
          right: 0 !important;
          bottom: 0 !important;
          inset: 0 !important;
          border-radius: 0 !important;
          box-shadow: none !important;
          z-index: 10000 !important;
          transition: transform 0.3s ease-in-out, height 0.3s ease-in-out;
          margin: 0 !important;
          padding: 0 !important;
          border: none !important;
        }

        :host([state="fullscreen"]) .window-container {
          position: relative;
          z-index: 1;
        }

        .window-container {
          width: 100%;
          height: 100%;
          display: flex;
          flex-direction: column;
          min-height: 0;
        }

        .titlebar {
          height: 40px;
          background: var(--window-titlebar);
          backdrop-filter: var(--menu-blur);
          -webkit-backdrop-filter: var(--menu-blur);
          display: flex;
          align-items: center;
          padding: 0 12px;
          cursor: move;
          user-select: none;
          border-bottom: 1px solid var(--separator-opaque);
        }

        :host([state="maximized"]) .titlebar {
          border-bottom: none;
        }

        :host([state="fullscreen"]) .titlebar {
          position: fixed;
          top: 28px;
          left: 0;
          width: 100vw;
          z-index: 10002;
          opacity: 0;
          pointer-events: none;
          transition: opacity 0.3s ease-in-out;
        }

        :host([state="fullscreen"][data-reveal-ui="true"]) .titlebar {
          opacity: 1;
          pointer-events: auto;
        }

        .controls {
          display: flex;
          gap: 8px;
        }

        .control-btn {
          width: 12px;
          height: 12px;
          border-radius: 50%;
          border: 0.5px solid rgba(0, 0, 0, 0.15);
          cursor: pointer;
          position: relative;
          box-shadow:
            inset 0 0.5px 1px rgba(255, 255, 255, 0.3),
            0 0.5px 1px rgba(0, 0, 0, 0.1);
        }

        /* T012: Traffic light hover symbols (016-gem3-optimizations) */
        .control-btn::before {
          content: '';
          position: absolute;
          top: 50%;
          left: 50%;
          transform: translate(-50%, -50%);
          font-size: 8px;
          font-weight: bold;
          color: rgba(0, 0, 0, 0.6);
          opacity: 0;
          transition: opacity 0.15s ease-in-out;
          pointer-events: none;
        }

        .controls:hover .control-btn.close::before {
          content: '×';
          opacity: 1;
        }

        .controls:hover .control-btn.minimize::before {
          content: '−';
          opacity: 1;
        }

        .controls:hover .control-btn.maximize::before {
          content: '+';
          opacity: 1;
        }

        .control-btn.close {
          background: linear-gradient(180deg, #ff5f57 0%, #e74c3c 100%);
        }
        .control-btn.minimize {
          background: linear-gradient(180deg, #ffbd2e 0%, #f39c12 100%);
        }
        .control-btn.maximize {
          background: linear-gradient(180deg, #28ca42 0%, #27ae60 100%);
        }

        .control-btn:hover {
          filter: brightness(1.05);
        }

        .control-btn:active {
          filter: brightness(0.95);
          box-shadow:
            inset 0 1px 2px rgba(0, 0, 0, 0.2),
            0 0.5px 1px rgba(0, 0, 0, 0.1);
        }

        .title {
          flex: 1;
          text-align: center;
          font-family: var(--font-family-system);
          font-size: var(--font-size-sm);
          font-weight: var(--font-weight-medium);
          color: var(--system-foreground);
        }

        .content {
          flex: 1;
          overflow: auto;
          background: var(--window-background);
          min-height: 0;
        }

        :host([state="fullscreen"]) .content {
          position: fixed;
          top: 0;
          left: 0;
          width: 100vw;
          height: 100vh;
          transition: transform 0.3s ease-in-out;
        }

        :host([state="fullscreen"][data-reveal-ui="true"]) .content {
          transform: translateY(68px);
          height: calc(100vh - 68px);
        }

        /* Resize handles (8 directions) */
        .resize-handle {
          position: absolute;
          background: transparent;
        }

        .resize-n, .resize-s { height: 4px; left: 0; right: 0; cursor: ns-resize; }
        .resize-e, .resize-w { width: 4px; top: 0; bottom: 0; cursor: ew-resize; }
        .resize-n { top: 0; }
        .resize-s { bottom: 0; }
        .resize-e { right: 0; }
        .resize-w { left: 0; }

        .resize-ne, .resize-nw, .resize-se, .resize-sw {
          width: 12px;
          height: 12px;
        }
        .resize-ne { top: 0; right: 0; cursor: nesw-resize; }
        .resize-nw { top: 0; left: 0; cursor: nwse-resize; }
        .resize-se { bottom: 0; right: 0; cursor: nwse-resize; }
        .resize-sw { bottom: 0; left: 0; cursor: nesw-resize; }
      </style>

      <div class="window-container" role="dialog" aria-modal="false">
        <div class="titlebar">
          <div class="controls">
            <button class="control-btn close" aria-label="Close" data-action="close"></button>
            <button class="control-btn minimize" aria-label="Minimize" data-action="minimize"></button>
            <button class="control-btn maximize" aria-label="Maximize" data-action="maximize"></button>
          </div>
          <div class="title" id="window-title"></div>
          <slot name="titlebar-extra"></slot>
        </div>

        <div class="content">
          <slot></slot>
        </div>

        <!-- Resize handles -->
        <div class="resize-handle resize-n" data-direction="n"></div>
        <div class="resize-handle resize-s" data-direction="s"></div>
        <div class="resize-handle resize-e" data-direction="e"></div>
        <div class="resize-handle resize-w" data-direction="w"></div>
        <div class="resize-handle resize-ne" data-direction="ne"></div>
        <div class="resize-handle resize-nw" data-direction="nw"></div>
        <div class="resize-handle resize-se" data-direction="se"></div>
        <div class="resize-handle resize-sw" data-direction="sw"></div>
      </div>
    `,this.shadowRoot.appendChild(e.content.cloneNode(!0)),this._updateTitle()}_setupEventListeners(){const e=this.shadowRoot.querySelector(".titlebar"),t=this.shadowRoot.querySelectorAll(".control-btn"),i=this.shadowRoot.querySelectorAll(".resize-handle");t.forEach(n=>{n.addEventListener("click",o=>this._handleControlClick(o))}),e.addEventListener("mousedown",n=>this._startDrag(n)),i.forEach(n=>{n.addEventListener("mousedown",o=>this._startResize(o))}),this._mouseMoveHandler=n=>this._handleMouseMove(n),this._mouseUpHandler=n=>this._handleMouseUp(n),this.addEventListener("click",()=>this.focus())}_cleanup(){document.removeEventListener("mousemove",this._mouseMoveHandler),document.removeEventListener("mouseup",this._mouseUpHandler),this._cleanupFullscreenMenuReveal()}_handleAttributeChange(e,t){switch(e){case"title":this._updateTitle();break;case"x":case"y":this._updatePosition();break;case"width":case"height":this._updateSize();break;case"z-index":this.style.zIndex=t;break}}_updateTitle(){const e=this.shadowRoot.querySelector(".title"),t=this.shadowRoot.querySelector(".window-container"),i=this.getAttribute("title")||"";e&&(e.textContent=i),t&&t.setAttribute("aria-label",i)}_updatePosition(){const e=parseInt(this.getAttribute("x")||"0",10),t=parseInt(this.getAttribute("y")||"0",10);this.style.left=`${e}px`,this.style.top=`${t}px`}_updateSize(){const e=parseInt(this.getAttribute("width")||"600",10),t=parseInt(this.getAttribute("height")||"400",10);this.style.width=`${e}px`,this.style.height=`${t}px`}_handleControlClick(e){switch(e.target.dataset.action){case"close":this.close();break;case"minimize":this.minimize();break;case"maximize":this.toggleFullscreen();break}e.stopPropagation()}_startDrag(e){e.target.classList.contains("control-btn")||(this._dragging=!0,this._dragStart={x:e.clientX-parseInt(this.getAttribute("x")||"0",10),y:e.clientY-parseInt(this.getAttribute("y")||"0",10)},document.addEventListener("mousemove",this._mouseMoveHandler),document.addEventListener("mouseup",this._mouseUpHandler),e.preventDefault())}_startResize(e){this._resizing=!0,this._resizeDirection=e.target.dataset.direction,this._resizeStart={x:e.clientX,y:e.clientY,width:parseInt(this.getAttribute("width")||"600",10),height:parseInt(this.getAttribute("height")||"400",10)},document.addEventListener("mousemove",this._mouseMoveHandler),document.addEventListener("mouseup",this._mouseUpHandler),e.preventDefault(),e.stopPropagation()}_handleMouseMove(e){if(this._dragging){const t=e.clientX-this._dragStart.x,i=e.clientY-this._dragStart.y;this.setPosition(t,i),this.dispatchEvent(new CustomEvent("window-drag",{detail:{x:t,y:i,windowId:this.id},bubbles:!0,composed:!0}))}else if(this._resizing){const t=e.clientX-this._resizeStart.x,i=e.clientY-this._resizeStart.y,n=this._resizeDirection;let o=this._resizeStart.width,a=this._resizeStart.height,d=parseInt(this.getAttribute("x")||"0",10),l=parseInt(this.getAttribute("y")||"0",10);n.includes("e")&&(o=this._resizeStart.width+t),n.includes("w")&&(o=this._resizeStart.width-t,d=parseInt(this.getAttribute("x")||"0",10)+t),n.includes("s")&&(a=this._resizeStart.height+i),n.includes("n")&&(a=this._resizeStart.height-i,l=parseInt(this.getAttribute("y")||"0",10)+i);const c=parseInt(this.getAttribute("min-width")||"400",10),p=parseInt(this.getAttribute("min-height")||"300",10);o<c&&(o=c,n.includes("w")&&(d=parseInt(this.getAttribute("x")||"0",10))),a<p&&(a=p,n.includes("n")&&(l=parseInt(this.getAttribute("y")||"0",10))),(n.includes("w")||n.includes("n"))&&this.setPosition(d,l),this.setSize(o,a),this.dispatchEvent(new CustomEvent("window-resize",{detail:{width:o,height:a,windowId:this.id},bubbles:!0,composed:!0}))}}_handleMouseUp(){this._dragging=!1,this._resizing=!1,document.removeEventListener("mousemove",this._mouseMoveHandler),document.removeEventListener("mouseup",this._mouseUpHandler)}}customElements.define("likes-window",te);class ie extends HTMLElement{static get observedAttributes(){return["position","size","magnification-enabled","magnification-max-size","auto-hide"]}constructor(){super(),this.attachShadow({mode:"open"}),this._icons=[],this._updatePromise=null,this._resolveUpdate=null}connectedCallback(){this.hasAttribute("position")||this.setAttribute("position","bottom"),this.hasAttribute("size")||this.setAttribute("size","64"),this._render(),this._setupEventListeners(),this._applyAttributes()}disconnectedCallback(){this._cleanup()}attributeChangedCallback(e,t,i){t!==i&&this._handleAttributeChange(e,i)}get updateComplete(){return this._updatePromise||(this._updatePromise=new Promise(e=>{this._resolveUpdate=e}),Promise.resolve().then(()=>{this._resolveUpdate&&(this._resolveUpdate(),this._updatePromise=null,this._resolveUpdate=null)})),this._updatePromise}addIcon(e,t=!1){const i={id:e.id,label:e.label||e.name,iconUrl:e.iconUrl,pinned:t,running:e.running||!1,badge:e.badge||null};this._icons.push(i),this._renderIcons()}removeIcon(e){this._icons=this._icons.filter(t=>t.id!==e),this._renderIcons()}reorderIcon(e,t){const i=this._icons.findIndex(o=>o.id===e);if(i===-1)return;const[n]=this._icons.splice(i,1);this._icons.splice(t,0,n),this._renderIcons(),this.dispatchEvent(new CustomEvent("dock-icon-reorder",{detail:{iconId:e,newPosition:t},bubbles:!0,composed:!0}))}updateIcon(e,t){const i=this._icons.find(n=>n.id===e);i&&(Object.assign(i,t),this._renderIcons())}_render(){const e=document.createElement("template");e.innerHTML=`
      <style>
        :host {
          display: flex;
          align-items: center;
          justify-content: center;
          gap: var(--spacing-2);
          padding: var(--spacing-3);
          background: var(--dock-background);
          backdrop-filter: var(--dock-blur);
          -webkit-backdrop-filter: var(--dock-blur);
          border: 1px solid var(--dock-border);
          border-radius: var(--radius-dock);
          box-shadow: var(--shadow-dock);
          z-index: var(--z-fixed);
          --dock-icon-size: 64px;
          --dock-icon-max-size: 128px;
          /* T014: CSS containment for performance optimization */
          contain: layout style paint;
        }

        :host([position="bottom"]) {
          position: fixed;
          bottom: var(--spacing-2);
          left: 50%;
          transform: translateX(-50%);
          flex-direction: row;
        }

        :host([position="left"]) {
          position: fixed;
          left: var(--spacing-2);
          top: 50%;
          transform: translateY(-50%);
          flex-direction: column;
        }

        :host([position="right"]) {
          position: fixed;
          right: var(--spacing-2);
          top: 50%;
          transform: translateY(-50%);
          flex-direction: column;
        }

        :host([auto-hide]) {
          transition: transform var(--duration-medium) var(--easing-standard);
        }

        :host([auto-hide]:not(:hover)) {
          transform: translateX(-50%) translateY(calc(100% + var(--spacing-2)));
        }

        :host([position="left"][auto-hide]:not(:hover)) {
          transform: translateY(-50%) translateX(calc(-100% - var(--spacing-2)));
        }

        :host([position="right"][auto-hide]:not(:hover)) {
          transform: translateY(-50%) translateX(calc(100% + var(--spacing-2)));
        }

        .dock-container {
          display: flex;
          gap: var(--spacing-2);
          align-items: center;
          justify-content: center;
          position: relative;
        }

        .icons-container {
          display: flex;
          gap: var(--spacing-2);
          align-items: center;
          justify-content: center;
        }

        :host([position="bottom"]) .dock-container,
        :host([position="top"]) .dock-container {
          flex-direction: row;
        }

        :host([position="bottom"]) .icons-container,
        :host([position="top"]) .icons-container {
          flex-direction: row;
        }

        :host([position="left"]) .dock-container,
        :host([position="right"]) .dock-container {
          flex-direction: column;
        }

        :host([position="left"]) .icons-container,
        :host([position="right"]) .icons-container {
          flex-direction: column;
        }

        .dock-icon {
          width: var(--dock-icon-size);
          height: var(--dock-icon-size);
          position: relative;
          display: flex;
          align-items: center;
          justify-content: center;
          cursor: pointer;
          transition: all var(--duration-fast) var(--easing-spring);
          transform-origin: center bottom;
          margin: 0 2px;
        }

        :host([position="left"]) .dock-icon,
        :host([position="right"]) .dock-icon {
          transform-origin: center center;
          margin: 2px 0;
        }

        .dock-icon img {
          width: 100%;
          height: 100%;
          object-fit: contain;
          border-radius: var(--radius-lg);
        }

        :host([magnification-enabled]) .dock-icon {
          --scale: 1;
          transform: scale(var(--scale)) translateY(calc((var(--scale) - 1) * -20px));
        }

        :host([magnification-enabled][position="left"]) .dock-icon,
        :host([magnification-enabled][position="right"]) .dock-icon {
          transform: scale(var(--scale));
        }

        .running-indicator {
          position: absolute;
          bottom: -8px;
          left: 50%;
          transform: translateX(-50%);
          width: 4px;
          height: 4px;
          border-radius: 50%;
          background: var(--dock-indicator);
          display: none;
        }

        .dock-icon[data-running="true"] .running-indicator {
          display: block;
        }

        .badge {
          position: absolute;
          top: -4px;
          right: -4px;
          min-width: 18px;
          height: 18px;
          background: var(--badge-background);
          color: var(--badge-foreground);
          border-radius: var(--radius-badge);
          font-size: var(--font-size-2xs);
          font-weight: var(--font-weight-bold);
          display: none;
          align-items: center;
          justify-content: center;
          padding: 0 6px;
        }

        .dock-icon[data-badge]:not([data-badge=""]):not([data-badge="0"]) .badge {
          display: flex;
        }

        .separator {
          width: 1px;
          height: 48px;
          background: var(--dock-separator);
          margin: 0 var(--spacing-1);
        }

        :host([position="left"]) .separator,
        :host([position="right"]) .separator {
          width: 48px;
          height: 1px;
          margin: var(--spacing-1) 0;
        }
      </style>

      <div class="dock-container" role="navigation" aria-label="Dock">
        <div class="icons-container"></div>
      </div>
    `,this.shadowRoot.appendChild(e.content.cloneNode(!0))}_setupEventListeners(){this.shadowRoot.querySelector(".dock-container");const e=this.shadowRoot.querySelector(".icons-container");this._clickHandler=t=>{const i=t.target.closest(".dock-icon");i&&this._handleIconClick(i)},e.addEventListener("click",this._clickHandler),this._contextMenuHandler=t=>{const i=t.target.closest(".dock-icon");i&&(t.preventDefault(),this._handleIconContextMenu(i,t))},e.addEventListener("contextmenu",this._contextMenuHandler),this._mouseMoveHandler=t=>{this.hasAttribute("magnification-enabled")&&this._handleMagnification(t)},this._mouseLeaveHandler=()=>{this._resetMagnification()},e.addEventListener("mousemove",this._mouseMoveHandler),e.addEventListener("mouseleave",this._mouseLeaveHandler)}_cleanup(){const e=this.shadowRoot.querySelector(".icons-container");e&&this._clickHandler&&e.removeEventListener("click",this._clickHandler),e&&this._contextMenuHandler&&e.removeEventListener("contextmenu",this._contextMenuHandler),e&&this._mouseMoveHandler&&e.removeEventListener("mousemove",this._mouseMoveHandler),e&&this._mouseLeaveHandler&&e.removeEventListener("mouseleave",this._mouseLeaveHandler)}_handleAttributeChange(e,t){switch(e){case"position":this._updatePosition(t);break;case"size":this.style.setProperty("--dock-icon-size",`${t}px`);break;case"magnification-max-size":this.style.setProperty("--dock-icon-max-size",`${t}px`);break;case"magnification-enabled":t!==null&&this._setupMagnificationListeners();break}}_setupMagnificationListeners(){const e=this.shadowRoot.querySelector(".icons-container");e&&(this._mouseMoveHandler&&e.removeEventListener("mousemove",this._mouseMoveHandler),this._mouseLeaveHandler&&e.removeEventListener("mouseleave",this._mouseLeaveHandler),this._mouseMoveHandler=t=>{this.hasAttribute("magnification-enabled")&&this._handleMagnification(t)},this._mouseLeaveHandler=()=>{this._resetMagnification()},e.addEventListener("mousemove",this._mouseMoveHandler),e.addEventListener("mouseleave",this._mouseLeaveHandler))}_applyAttributes(){const e=this.getAttribute("position")||"bottom",t=this.getAttribute("size")||"64",i=this.getAttribute("magnification-max-size")||"128";this._updatePosition(e),this.style.setProperty("--dock-icon-size",`${t}px`),this.style.setProperty("--dock-icon-max-size",`${i}px`)}_updatePosition(e){const t=this.shadowRoot.querySelector(".dock-container");t&&(t.classList.remove("position-bottom","position-left","position-right"),t.classList.add(`position-${e}`))}_renderIcons(){const e=this.shadowRoot.querySelector(".icons-container");e&&(e.innerHTML="",this._icons.forEach((t,i)=>{const n=document.createElement("div");n.className="dock-icon",n.dataset.iconId=t.id,t.running&&(n.dataset.running="true"),t.badge&&(n.dataset.badge=t.badge),n.setAttribute("role","button"),n.setAttribute("aria-label",t.label),n.setAttribute("tabindex","0"),n.innerHTML=`
        <img src="${t.iconUrl}" alt="${t.label}" />
        <div class="icon-fallback" style="display: none; width: 100%; height: 100%; align-items: center; justify-content: center; background: linear-gradient(135deg, #667eea 0%, #764ba2 100%); border-radius: var(--radius-lg); color: white; font-size: 32px; font-weight: bold;">${t.label.charAt(0)}</div>
        <div class="running-indicator"></div>
        <div class="badge">${t.badge||""}</div>
      `,e.appendChild(n)}))}_handleIconClick(e){const t=e.dataset.iconId;this.dispatchEvent(new CustomEvent("dock-icon-click",{detail:{iconId:t},bubbles:!0,composed:!0}))}_handleIconContextMenu(e,t){const i=e.dataset.iconId;this.dispatchEvent(new CustomEvent("dock-icon-contextmenu",{detail:{iconId:i,x:t.clientX,y:t.clientY},bubbles:!0,composed:!0}))}_handleMagnification(e){const t=this.shadowRoot.querySelector(".icons-container"),i=Array.from(t.querySelectorAll(".dock-icon"));if(i.length===0)return;const n=this.getAttribute("position")||"bottom",o=n==="bottom"||n==="top";i.forEach(a=>{const d=a.getBoundingClientRect(),l=o?d.left+d.width/2:d.top+d.height/2,c=o?e.clientX:e.clientY,p=Math.abs(c-l),u=2,h=1,v=150;let _;if(p<v){const S=p/v;_=u-(u-h)*Math.pow(S,.7)}else _=h;a.style.willChange||(a.style.willChange="transform"),a.style.setProperty("--scale",_.toFixed(3))})}_resetMagnification(){this.shadowRoot.querySelector(".icons-container").querySelectorAll(".dock-icon").forEach(i=>{i.style.willChange="",i.style.setProperty("--scale","1")})}}customElements.define("likes-dock",ie);class ne{constructor(){this._listeners=new Map}on(e,t){return this._listeners.has(e)||this._listeners.set(e,new Set),this._listeners.get(e).add(t),()=>{this.off(e,t)}}once(e,t){const i=(...n)=>{t(...n),this.off(e,i)};return this.on(e,i)}off(e,t){const i=this._listeners.get(e);i&&(i.delete(t),i.size===0&&this._listeners.delete(e))}emit(e,t){const i=this._listeners.get(e);i&&i.forEach(o=>{try{o(t,e)}catch(a){console.error(`Error in event listener for "${e}":`,a)}});const n=this._listeners.get("*");n&&n.forEach(o=>{try{o(t,e)}catch(a){console.error(`Error in wildcard listener for "${e}":`,a)}}),this._listeners.forEach((o,a)=>{a.endsWith(":*")&&e.startsWith(a.slice(0,-1))&&o.forEach(d=>{try{d(t,e)}catch(l){console.error(`Error in pattern listener "${a}" for "${e}":`,l)}})})}clear(e){e?this._listeners.delete(e):this._listeners.clear()}listenerCount(e){const t=this._listeners.get(e);return t?t.size:0}eventNames(){return Array.from(this._listeners.keys())}}const s=new ne,it=Object.freeze(Object.defineProperty({__proto__:null,default:s},Symbol.toStringTag,{value:"Module"}));class oe{constructor(){this._extensions=new Map,this._order=[]}registerExtension(e){const{id:t,appId:i,componentTag:n,priority:o=0,props:a={},persistent:d=!1}=e;if(!t||!i||!n)return console.error("Extension registration missing required fields:",e),!1;if(this._extensions.has(t))return console.warn(`Extension ${t} already registered`),!1;const l={id:t,appId:i,componentTag:n,priority:o,props:a,persistent:d,registeredAt:Date.now()};return this._extensions.set(t,l),this._updateOrder(),s.emit("extension:registered",{extension:l}),console.log(`[ExtensionManager] Registered extension: ${t} from app: ${i} (persistent: ${d})`),!0}unregisterExtension(e){return this._extensions.get(e)?(this._extensions.delete(e),this._updateOrder(),s.emit("extension:unregistered",{extensionId:e}),console.log(`[ExtensionManager] Unregistered extension: ${e}`),!0):(console.warn(`Extension ${e} not found`),!1)}unregisterAppExtensions(e,t=!1){const i=[];this._extensions.forEach((n,o)=>{n.appId===e&&(!n.persistent||t)&&i.push(o)}),i.forEach(n=>this.unregisterExtension(n))}getExtensions(){return this._order.map(e=>this._extensions.get(e))}getExtension(e){return this._extensions.get(e)||null}updateExtensionProps(e,t){const i=this._extensions.get(e);if(!i){console.warn(`Extension ${e} not found`);return}i.props={...i.props,...t},s.emit("extension:updated",{extensionId:e,props:t})}_updateOrder(){this._order=Array.from(this._extensions.values()).sort((e,t)=>e.priority-t.priority).map(e=>e.id),s.emit("extension:order-changed",{order:this._order})}}const G=new oe;s.on("app:terminated",({appId:r})=>{G.unregisterAppExtensions(r)});class se extends HTMLElement{constructor(){super(),this.attachShadow({mode:"open"}),this._appName="Finder",this._currentAppId="finder",this._currentAppComponent=null,this._activeMenu=null,this._clickHandler=null,this._updatePromise=null,this._resolveUpdate=null}connectedCallback(){this._render(),this._updateAppMenus(),this._renderExtensions(),this._setupEventListeners(),this._markUpdateComplete()}get updateComplete(){return this._updatePromise||(this._updatePromise=new Promise(e=>{this._resolveUpdate=e}),Promise.resolve().then(()=>{this._resolveUpdate&&(this._resolveUpdate(),this._updatePromise=null,this._resolveUpdate=null)})),this._updatePromise}_markUpdateComplete(){this._resolveUpdate&&(this._resolveUpdate(),this._updatePromise=null,this._resolveUpdate=null)}disconnectedCallback(){this._cleanup()}setAppName(e){this._appName=e,this._updateAppName()}setActiveApp(e,t,i=null){this._currentAppId=e,this._appName=t,this._currentAppComponent=i,this._updateAppName(),this._updateAppMenus()}_render(){const e=document.createElement("template");e.innerHTML=`
      <style>
        :host {
          display: block;
          position: fixed;
          top: 0;
          left: 0;
          right: 0;
          height: 28px;
          background: var(--window-titlebar);
          backdrop-filter: var(--menu-blur);
          -webkit-backdrop-filter: var(--menu-blur);
          border-bottom: 1px solid var(--separator-opaque);
          z-index: var(--z-fixed);
          font-family: var(--font-family-system);
          font-size: var(--font-size-sm);
          color: var(--system-foreground);
        }

        .menubar-container {
          display: flex;
          align-items: center;
          height: 100%;
          padding: 0 var(--spacing-3);
          gap: var(--spacing-4);
        }

        .left-section {
          display: flex;
          align-items: center;
          gap: var(--spacing-4);
          flex: 1;
        }

        #app-menus {
          display: flex;
          align-items: center;
          gap: var(--spacing-4);
        }

        .right-section {
          display: flex;
          align-items: center;
          gap: var(--spacing-3);
          margin-left: auto;
        }

        #extensions-container {
          display: flex;
          align-items: center;
          gap: var(--spacing-2);
        }

        #extensions-container > * {
          display: flex;
          align-items: center;
          height: 28px;
        }

        .apple-logo {
          font-size: 16px;
          cursor: pointer;
          user-select: none;
        }

        .app-name {
          font-weight: var(--font-weight-semibold);
          padding: 4px 12px;
          border-radius: 6px;
          background: var(--menu-app-name-bg);
        }

        .menu-item {
          cursor: pointer;
          user-select: none;
          padding: 2px 8px;
          border-radius: var(--radius-sm);
          transition: background var(--duration-fast) var(--easing-standard);
        }

        .menu-item:hover {
          background: var(--menu-item-hover);
        }

        .status-item {
          font-size: var(--font-size-xs);
          user-select: none;
        }

        .time {
          min-width: 65px;
          text-align: right;
        }

        .menu-item {
          position: relative;
        }

        .menu-item.active {
          background: var(--accent-color);
          color: white;
        }

        .dropdown-menu {
          display: none;
          position: absolute;
          top: 100%;
          left: 0;
          min-width: 220px;
          background: var(--menu-background);
          backdrop-filter: var(--menu-blur);
          -webkit-backdrop-filter: var(--menu-blur);
          border: 1px solid var(--separator-opaque);
          border-radius: var(--radius-md);
          box-shadow: var(--shadow-lg);
          padding: var(--spacing-1) 0;
          z-index: calc(var(--z-fixed) + 1);
          margin-top: 2px;
        }

        .dropdown-menu.visible {
          display: block;
        }

        .menu-option {
          padding: var(--spacing-2) var(--spacing-4);
          cursor: pointer;
          user-select: none;
          display: flex;
          align-items: center;
          justify-content: space-between;
          color: var(--system-foreground);
        }

        .menu-option:hover {
          background: var(--menu-item-selected);
          color: white;
        }

        .menu-option.disabled {
          opacity: 0.5;
          cursor: not-allowed;
        }

        .menu-option.disabled:hover {
          background: transparent;
          color: var(--system-foreground);
        }

        .menu-separator {
          height: 1px;
          background: var(--menu-separator);
          margin: var(--spacing-1) var(--spacing-2);
        }

        .menu-shortcut {
          margin-left: var(--spacing-6);
          font-size: var(--font-size-xs);
          color: var(--system-foreground-tertiary);
        }
      </style>

      <div class="menubar-container">
        <div class="left-section">
          <div class="menu-item" data-menu="apple">
            <span class="apple-logo">🍎</span>
            <div class="dropdown-menu" id="apple-menu">
              <div class="menu-option" data-action="about">About This Mac</div>
              <div class="menu-separator"></div>
              <div class="menu-option" data-action="system-settings">
                System Settings...
                <span class="menu-shortcut">⌘,</span>
              </div>
              <div class="menu-separator"></div>
              <div class="menu-option" data-action="sleep">Sleep</div>
              <div class="menu-option" data-action="restart">Restart...</div>
              <div class="menu-option" data-action="shutdown">Shut Down...</div>
            </div>
          </div>
          <div class="menu-item" data-menu="app" id="app-menu-item">
            <span class="app-name">${this._appName}</span>
            <div class="dropdown-menu" id="app-menu">
              <!-- App menu items will be inserted here -->
            </div>
          </div>
          <div id="app-menus">
            <!-- App-specific menus will be inserted here -->
          </div>
        </div>
        <div class="right-section">
          <div id="extensions-container"></div>
          <div class="status-item time" id="clock"></div>
        </div>
      </div>
    `,this.shadowRoot.appendChild(e.content.cloneNode(!0)),this._startClock()}_updateAppName(){const e=this.shadowRoot.querySelector(".app-name");e&&(e.textContent=this._appName),this._updateAppMenu()}_updateAppMenu(){const e=this.shadowRoot.getElementById("app-menu");if(!e)return;const t=this._appName,i=this._currentAppId==="finder";e.innerHTML=`
      <div class="menu-option" data-action="app-about">About ${t}</div>
      <div class="menu-separator"></div>
      <div class="menu-option" data-action="app-settings">
        Settings...
        <span class="menu-shortcut">⌘,</span>
      </div>
      <div class="menu-separator"></div>
      <div class="menu-option" data-action="app-hide">
        Hide ${t}
        <span class="menu-shortcut">⌘H</span>
      </div>
      <div class="menu-option" data-action="hide-others">
        Hide Others
        <span class="menu-shortcut">⌥⌘H</span>
      </div>
      <div class="menu-option disabled" data-action="show-all">Show All</div>
      <div class="menu-separator"></div>
      <div class="menu-option ${i?"disabled":""}" data-action="app-quit">
        Quit ${t}
        <span class="menu-shortcut">⌘Q</span>
      </div>
    `,e.querySelectorAll(".menu-option").forEach(o=>{o.addEventListener("click",a=>{if(o.classList.contains("disabled"))return;a.stopPropagation();const d=o.dataset.action;d&&(this._handleMenuAction(d),this._closeAllMenus())})})}_updateAppNameFromAppId(e){const i={finder:"Finder",safari:"Safari",mail:"Mail",messages:"Messages",music:"Music","event-log":"System Event Log",about:"About This Mac"}[e]||e;this.setActiveApp(e,i)}_getAppMenus(e){const t={finder:{file:[{label:"New Finder Window",action:"new-window",shortcut:"⌘N"},{label:"New Folder",action:"new-folder",shortcut:"⌘⇧N"},{separator:!0},{label:"Close Window",action:"close-window",shortcut:"⌘W"}],edit:[{label:"Undo",action:"undo",shortcut:"⌘Z",disabled:!0},{label:"Redo",action:"redo",shortcut:"⌘⇧Z",disabled:!0},{separator:!0},{label:"Cut",action:"cut",shortcut:"⌘X",disabled:!0},{label:"Copy",action:"copy",shortcut:"⌘C",disabled:!0},{label:"Paste",action:"paste",shortcut:"⌘V",disabled:!0}],view:[{label:"as Icons",action:"view-icons"},{label:"as List",action:"view-list"},{label:"as Columns",action:"view-columns"},{separator:!0},{label:"Show Toolbar",action:"show-toolbar"},{label:"Show Sidebar",action:"show-sidebar"}],window:[{label:"Minimize",action:"minimize",shortcut:"⌘M"},{label:"Zoom",action:"zoom"},{separator:!0},{label:"Bring All to Front",action:"bring-all-to-front"}],help:[{label:"Search",action:"search-help"},{separator:!0},{label:"Event Log",action:"event-log",shortcut:"⌘⇧L"}]},safari:{file:[{label:"New Window",action:"new-window",shortcut:"⌘N"},{label:"New Tab",action:"new-tab",shortcut:"⌘T"},{separator:!0},{label:"Close Window",action:"close-window",shortcut:"⌘W"},{label:"Close Tab",action:"close-tab",shortcut:"⌘W"}],edit:[{label:"Undo",action:"undo",shortcut:"⌘Z",disabled:!0},{label:"Redo",action:"redo",shortcut:"⌘⇧Z",disabled:!0},{separator:!0},{label:"Cut",action:"cut",shortcut:"⌘X",disabled:!0},{label:"Copy",action:"copy",shortcut:"⌘C",disabled:!0},{label:"Paste",action:"paste",shortcut:"⌘V",disabled:!0},{separator:!0},{label:"Find...",action:"find",shortcut:"⌘F"}],view:[{label:"Show Toolbar",action:"show-toolbar"},{label:"Show Tab Bar",action:"show-tab-bar"},{separator:!0},{label:"Reload Page",action:"reload",shortcut:"⌘R"}],window:[{label:"Minimize",action:"minimize",shortcut:"⌘M"},{label:"Zoom",action:"zoom"},{separator:!0},{label:"Bring All to Front",action:"bring-all-to-front"}],help:[{label:"Safari Help",action:"app-help"},{separator:!0},{label:"Event Log",action:"event-log",shortcut:"⌘⇧L"}]},mail:{file:[{label:"New Message",action:"new-message",shortcut:"⌘N"},{separator:!0},{label:"Close Window",action:"close-window",shortcut:"⌘W"}],edit:[{label:"Undo",action:"undo",shortcut:"⌘Z",disabled:!0},{label:"Redo",action:"redo",shortcut:"⌘⇧Z",disabled:!0},{separator:!0},{label:"Cut",action:"cut",shortcut:"⌘X",disabled:!0},{label:"Copy",action:"copy",shortcut:"⌘C",disabled:!0},{label:"Paste",action:"paste",shortcut:"⌘V",disabled:!0}],view:[{label:"Show Mailbox List",action:"show-mailbox"},{label:"Show Preview",action:"show-preview"}],window:[{label:"Minimize",action:"minimize",shortcut:"⌘M"},{label:"Zoom",action:"zoom"},{separator:!0},{label:"Bring All to Front",action:"bring-all-to-front"}],help:[{label:"Mail Help",action:"app-help"},{separator:!0},{label:"Event Log",action:"event-log",shortcut:"⌘⇧L"}]},messages:{file:[{label:"New Message",action:"new-message",shortcut:"⌘N"},{separator:!0},{label:"Close Window",action:"close-window",shortcut:"⌘W"}],edit:[{label:"Undo",action:"undo",shortcut:"⌘Z",disabled:!0},{label:"Redo",action:"redo",shortcut:"⌘⇧Z",disabled:!0},{separator:!0},{label:"Cut",action:"cut",shortcut:"⌘X",disabled:!0},{label:"Copy",action:"copy",shortcut:"⌘C",disabled:!0},{label:"Paste",action:"paste",shortcut:"⌘V",disabled:!0}],view:[{label:"Show Sidebar",action:"show-sidebar"}],window:[{label:"Minimize",action:"minimize",shortcut:"⌘M"},{label:"Zoom",action:"zoom"},{separator:!0},{label:"Bring All to Front",action:"bring-all-to-front"}],help:[{label:"Messages Help",action:"app-help"},{separator:!0},{label:"Event Log",action:"event-log",shortcut:"⌘⇧L"}]},music:{file:[{label:"New Playlist",action:"new-playlist",shortcut:"⌘N"},{separator:!0},{label:"Close Window",action:"close-window",shortcut:"⌘W"}],edit:[{label:"Undo",action:"undo",shortcut:"⌘Z",disabled:!0},{label:"Redo",action:"redo",shortcut:"⌘⇧Z",disabled:!0},{separator:!0},{label:"Cut",action:"cut",shortcut:"⌘X",disabled:!0},{label:"Copy",action:"copy",shortcut:"⌘C",disabled:!0},{label:"Paste",action:"paste",shortcut:"⌘V",disabled:!0}],view:[{label:"Show Sidebar",action:"show-sidebar"},{label:"Show MiniPlayer",action:"show-miniplayer"}],window:[{label:"Minimize",action:"minimize",shortcut:"⌘M"},{label:"Zoom",action:"zoom"},{separator:!0},{label:"Bring All to Front",action:"bring-all-to-front"}],help:[{label:"Music Help",action:"app-help"},{separator:!0},{label:"Event Log",action:"event-log",shortcut:"⌘⇧L"}]}},i={file:[{label:"New Window",action:"new-window",shortcut:"⌘N"},{separator:!0},{label:"Close Window",action:"close-window",shortcut:"⌘W"}],window:[{label:"Minimize",action:"minimize",shortcut:"⌘M"},{label:"Zoom",action:"zoom"},{separator:!0},{label:"Bring All to Front",action:"bring-all-to-front"}],help:[{label:"Event Log",action:"event-log",shortcut:"⌘⇧L"}]};return t[e]||i}_updateAppMenus(){const e=this.shadowRoot.getElementById("app-menus");if(!e)return;let t;this._currentAppComponent&&typeof this._currentAppComponent.getMenus=="function"?t=this._currentAppComponent.getMenus():t=this._getAppMenus(this._currentAppId),e.innerHTML="",Object.entries(t).forEach(([i,n])=>{const o=document.createElement("div");o.className="menu-item",o.dataset.menu=i;const a=i.charAt(0).toUpperCase()+i.slice(1);o.textContent=a;const d=document.createElement("div");d.className="dropdown-menu",d.id=`${i}-menu`,n.forEach(l=>{if(l.separator){const c=document.createElement("div");c.className="menu-separator",d.appendChild(c)}else{const c=document.createElement("div");c.className="menu-option"+(l.disabled?" disabled":""),c.dataset.action=l.action;const p=document.createElement("span");if(p.textContent=l.label,c.appendChild(p),l.shortcut){const u=document.createElement("span");u.className="menu-shortcut",u.textContent=l.shortcut,c.appendChild(u)}d.appendChild(c)}}),o.appendChild(d),e.appendChild(o)}),this._attachMenuEventListeners()}_attachMenuEventListeners(){this.shadowRoot.querySelectorAll("#app-menus .menu-item").forEach(i=>{i.addEventListener("click",n=>{n.stopPropagation();const o=i.dataset.menu;o&&this._toggleMenu(o,i)})}),this.shadowRoot.querySelectorAll("#app-menus .menu-option").forEach(i=>{i.addEventListener("click",n=>{if(i.classList.contains("disabled"))return;n.stopPropagation();const o=i.dataset.action;o&&(this._handleMenuAction(o),this._closeAllMenus())})})}_setupEventListeners(){s.on("extension:registered",()=>this._renderExtensions()),s.on("extension:unregistered",()=>this._renderExtensions()),s.on("extension:updated",()=>this._renderExtensions());const e=this.shadowRoot.querySelector('[data-menu="apple"]');e&&(e.addEventListener("click",n=>{n.stopPropagation(),this._toggleMenu("apple",e)}),e.querySelectorAll(".menu-option").forEach(n=>{n.addEventListener("click",o=>{if(n.classList.contains("disabled"))return;o.stopPropagation();const a=n.dataset.action;a&&(this._handleMenuAction(a),this._closeAllMenus())})}));const t=this.shadowRoot.getElementById("app-menu-item");t&&t.addEventListener("click",i=>{i.stopPropagation(),this._toggleMenu("app",t)}),this._clickHandler=i=>{this.shadowRoot.contains(i.target)||this._closeAllMenus()},document.addEventListener("click",this._clickHandler),s.on("window:created",()=>this._updateWindowMenu()),s.on("window:closed",()=>this._updateWindowMenu()),s.on("window:focused",i=>{if(this._updateWindowMenu(),i&&i.appId){const o={finder:"Finder",safari:"Safari",mail:"Mail",messages:"Messages",music:"Music","event-log":"System Event Log",about:"About This Mac"}[i.appId]||i.appId;this.setActiveApp(i.appId,o,i.appComponent)}}),s.on("app:focus",i=>{i&&i.appId&&this._updateAppNameFromAppId(i.appId)}),s.on("desktop-click",()=>{this.setActiveApp("finder","Finder")})}_cleanup(){this._clickHandler&&document.removeEventListener("click",this._clickHandler),this._clockInterval&&clearInterval(this._clockInterval)}_toggleMenu(e,t){const i=this.shadowRoot.getElementById(`${e}-menu`);this._activeMenu===e?this._closeAllMenus():(this._closeAllMenus(),i.classList.add("visible"),t.classList.add("active"),this._activeMenu=e,e==="window"&&this._updateWindowMenu())}_closeAllMenus(){this.shadowRoot.querySelectorAll(".dropdown-menu").forEach(i=>i.classList.remove("visible")),this.shadowRoot.querySelectorAll(".menu-item").forEach(i=>i.classList.remove("active")),this._activeMenu=null}_handleMenuAction(e){switch(e){case"about":s.emit("menu:about");break;case"system-settings":s.emit("menu:system-settings");break;case"sleep":case"restart":case"shutdown":s.emit("menu:power",{action:e});break;case"app-about":s.emit("menu:app-about",{appId:this._currentAppId,appName:this._appName});break;case"app-settings":s.emit("menu:app-settings",{appId:this._currentAppId});break;case"app-hide":s.emit("menu:app-hide",{appId:this._currentAppId});break;case"hide-others":s.emit("menu:hide-others",{appId:this._currentAppId});break;case"show-all":s.emit("menu:show-all");break;case"app-quit":s.emit("menu:app-quit",{appId:this._currentAppId,appName:this._appName});break;case"new-window":s.emit("menu:new-window");break;case"new-folder":s.emit("menu:new-folder");break;case"close-window":s.emit("menu:close-window");break;case"minimize":s.emit("menu:minimize");break;case"zoom":s.emit("menu:zoom");break;case"bring-all-to-front":s.emit("menu:bring-all-to-front");break;case"event-log":s.emit("menu:event-log");break;default:this._currentAppComponent&&typeof this._currentAppComponent.handleMenuAction=="function"?this._currentAppComponent.handleMenuAction(e):console.log(`Menu action: ${e}`)}}_updateWindowMenu(){}_renderExtensions(){const e=this.shadowRoot.getElementById("extensions-container");if(!e)return;e.innerHTML="",G.getExtensions().forEach(i=>{try{const n=document.createElement(i.componentTag);i.props&&Object.keys(i.props).forEach(o=>{n.setAttribute(o,i.props[o])}),n.dataset.extensionId=i.id,e.appendChild(n)}catch(n){console.error(`Failed to render extension ${i.id}:`,n)}})}_startClock(){const e=this.shadowRoot.getElementById("clock"),t=()=>{const i=new Date,n=i.getHours(),o=i.getMinutes().toString().padStart(2,"0"),a=n>=12?"PM":"AM",l=`${n%12||12}:${o} ${a}`;e&&(e.textContent=l)};e&&(e.style.cursor="pointer",e.addEventListener("click",i=>{console.log("[MenuBar] Clock clicked - toggling notification center"),i.preventDefault(),i.stopPropagation();const n=document.getElementById("notification-center");console.log("[MenuBar] Notification center element:",n),n?n.toggle():console.error("[MenuBar] Notification center not found!")})),t(),this._clockInterval=setInterval(t,1e3)}}customElements.define("likes-menubar",se);class ae extends HTMLElement{static get observedAttributes(){return["notification-id","title","message","icon-url","action-label","auto-dismiss-duration","style","critical"]}constructor(){super(),this.attachShadow({mode:"open"}),this._dismissTimer=null,this._isPaused=!1,this._remainingTime=null,this._updatePromise=null,this._resolveUpdate=null}connectedCallback(){this._render(),this._setupEventListeners(),this._startAutoDismiss(),this._playSlideInAnimation()}disconnectedCallback(){this._cleanup()}attributeChangedCallback(e,t,i){t!==i&&this._handleAttributeChange(e,i)}get updateComplete(){return this._updatePromise||(this._updatePromise=new Promise(e=>{this._resolveUpdate=e}),Promise.resolve().then(()=>{this._resolveUpdate&&(this._resolveUpdate(),this._updatePromise=null,this._resolveUpdate=null)})),this._updatePromise}dismiss(){this._cleanup(),this._playSlideOutAnimation(),this.dispatchEvent(new CustomEvent("notification-dismiss",{detail:{notificationId:this.getAttribute("notification-id")},bubbles:!0,composed:!0})),setTimeout(()=>{this.remove()},300)}_render(){const e=document.createElement("template");e.innerHTML=`
      <style>
        :host {
          display: block;
          position: fixed;
          top: var(--spacing-4);
          right: var(--spacing-4);
          z-index: var(--z-notification);
          min-width: 320px;
          max-width: 400px;
        }

        .banner-container {
          background: var(--notification-background);
          backdrop-filter: var(--backdrop-blur);
          -webkit-backdrop-filter: var(--backdrop-blur);
          border: 1px solid var(--notification-border);
          border-radius: var(--radius-notification);
          box-shadow: var(--shadow-notification);
          padding: var(--spacing-4);
          display: flex;
          gap: var(--spacing-3);
          align-items: flex-start;
          animation: notification-slide-in var(--duration-notification-slide) var(--easing-standard);
        }

        .banner-container.slide-out {
          animation: notification-slide-out var(--duration-notification-slide) var(--easing-standard);
        }

        .banner-container.style-alert {
          background: var(--accent-red);
          color: var(--system-background);
        }

        .notification-icon {
          width: 48px;
          height: 48px;
          border-radius: var(--radius-lg);
          object-fit: cover;
          flex-shrink: 0;
        }

        .notification-content {
          flex: 1;
          min-width: 0;
        }

        .notification-title {
          font-family: var(--font-family-system);
          font-size: var(--font-size-base);
          font-weight: var(--font-weight-semibold);
          color: var(--system-foreground);
          margin: 0 0 var(--spacing-1);
        }

        .style-alert .notification-title {
          color: inherit;
        }

        .notification-message {
          font-family: var(--font-family-system);
          font-size: var(--font-size-sm);
          color: var(--system-foreground-secondary);
          margin: 0;
          word-wrap: break-word;
        }

        .style-alert .notification-message {
          color: inherit;
          opacity: 0.9;
        }

        .actions {
          display: flex;
          gap: var(--spacing-2);
          margin-top: var(--spacing-2);
        }

        .action-button {
          font-family: var(--font-family-system);
          font-size: var(--font-size-sm);
          font-weight: var(--font-weight-medium);
          color: var(--accent-color);
          background: transparent;
          border: none;
          padding: var(--spacing-1) var(--spacing-2);
          border-radius: var(--radius-sm);
          cursor: pointer;
          transition: background var(--duration-fast) var(--easing-standard);
        }

        .action-button:hover {
          background: var(--fill-tertiary);
        }

        .style-alert .action-button {
          color: inherit;
        }

        .style-alert .action-button:hover {
          background: rgba(255, 255, 255, 0.2);
        }

        .close-button {
          width: 24px;
          height: 24px;
          border-radius: var(--radius-full);
          background: transparent;
          border: none;
          cursor: pointer;
          display: flex;
          align-items: center;
          justify-content: center;
          color: var(--system-foreground-tertiary);
          transition: background var(--duration-fast) var(--easing-standard);
          flex-shrink: 0;
        }

        .close-button:hover {
          background: var(--fill-tertiary);
        }

        .style-alert .close-button {
          color: inherit;
        }

        .style-alert .close-button:hover {
          background: rgba(255, 255, 255, 0.2);
        }

        .close-button::before {
          content: '×';
          font-size: 20px;
          line-height: 1;
        }
      </style>

      <div class="banner-container" role="alert" aria-live="polite">
        <img class="notification-icon" alt="" style="display: none;" />
        <div class="notification-content">
          <h3 class="notification-title"></h3>
          <p class="notification-message"></p>
          <div class="actions" style="display: none;">
            <button class="action-button"></button>
          </div>
        </div>
        <button class="close-button" aria-label="Close notification"></button>
      </div>
    `,this.shadowRoot.appendChild(e.content.cloneNode(!0)),this._updateContent()}_setupEventListeners(){const e=this.shadowRoot.querySelector(".banner-container"),t=this.shadowRoot.querySelector(".close-button"),i=this.shadowRoot.querySelector(".action-button");this._clickHandler=()=>{this.dispatchEvent(new CustomEvent("notification-click",{detail:{notificationId:this.getAttribute("notification-id")},bubbles:!0,composed:!0}))},e.addEventListener("click",this._clickHandler),this._closeHandler=n=>{n.stopPropagation(),this.dismiss()},t.addEventListener("click",this._closeHandler),this._actionHandler=n=>{n.stopPropagation(),this.dispatchEvent(new CustomEvent("notification-action",{detail:{notificationId:this.getAttribute("notification-id")},bubbles:!0,composed:!0})),this.dismiss()},i.addEventListener("click",this._actionHandler),this._mouseEnterHandler=()=>this._pauseAutoDismiss(),this._mouseLeaveHandler=()=>this._resumeAutoDismiss(),e.addEventListener("mouseenter",this._mouseEnterHandler),e.addEventListener("mouseleave",this._mouseLeaveHandler)}_cleanup(){this._dismissTimer&&(clearTimeout(this._dismissTimer),this._dismissTimer=null)}_handleAttributeChange(e,t){(e==="title"||e==="message"||e==="icon-url"||e==="action-label"||e==="style")&&this._updateContent()}_updateContent(){const e=this.shadowRoot.querySelector(".notification-title"),t=this.shadowRoot.querySelector(".notification-message"),i=this.shadowRoot.querySelector(".notification-icon"),n=this.shadowRoot.querySelector(".action-button"),o=this.shadowRoot.querySelector(".actions"),a=this.shadowRoot.querySelector(".banner-container");e&&(e.textContent=this.getAttribute("title")||""),t&&(t.textContent=this.getAttribute("message")||"");const d=this.getAttribute("icon-url");i&&(d?(i.src=d,i.style.display="block"):i.style.display="none");const l=this.getAttribute("action-label");n&&o&&(l?(n.textContent=l,o.style.display="flex"):o.style.display="none");const c=this.getAttribute("style");a&&a.classList.toggle("style-alert",c==="alert")}_startAutoDismiss(){const e=parseInt(this.getAttribute("auto-dismiss-duration")||"5000",10);e<=0||(this._remainingTime=e,this._dismissTimer=setTimeout(()=>{this.dismiss()},e))}_pauseAutoDismiss(){!this._dismissTimer||this._isPaused||(this._isPaused=!0,clearTimeout(this._dismissTimer),this._dismissTimer=null)}_resumeAutoDismiss(){!this._isPaused||!this._remainingTime||(this._isPaused=!1,this._dismissTimer=setTimeout(()=>{this.dismiss()},this._remainingTime))}_playSlideInAnimation(){const e=this.shadowRoot.querySelector(".banner-container");e&&e.classList.add("slide-in")}_playSlideOutAnimation(){const e=this.shadowRoot.querySelector(".banner-container");e&&e.classList.add("slide-out")}}customElements.define("notification-banner",ae);class re extends HTMLElement{static get observedAttributes(){return["open"]}constructor(){super(),this.attachShadow({mode:"open"}),this._notifications=[],this._updatePromise=null,this._resolveUpdate=null}connectedCallback(){this._render(),this._setupEventListeners()}disconnectedCallback(){this._cleanup()}attributeChangedCallback(e,t,i){t!==i&&e==="open"&&this._handleOpenChange(i!==null)}get updateComplete(){return this._updatePromise||(this._updatePromise=new Promise(e=>{this._resolveUpdate=e}),Promise.resolve().then(()=>{this._resolveUpdate&&(this._resolveUpdate(),this._updatePromise=null,this._resolveUpdate=null)})),this._updatePromise}open(){this.setAttribute("open","")}close(){this.removeAttribute("open")}toggle(){this.hasAttribute("open")?this.close():this.open()}addNotification(e){this._notifications.unshift({id:e.id,title:e.title,message:e.message,timestamp:e.timestamp||new Date().toISOString(),iconUrl:e.iconUrl}),this._renderNotifications()}clearAll(){this._notifications=[],this._renderNotifications()}removeNotification(e){this._notifications=this._notifications.filter(t=>t.id!==e),this._renderNotifications()}_render(){const e=document.createElement("template");e.innerHTML=`
      <style>
        :host {
          display: none;
          position: fixed;
          top: 0;
          right: 0;
          width: 100%;
          height: 100%;
          z-index: var(--z-notification);
        }

        :host([open]) {
          display: block;
        }

        .overlay {
          position: absolute;
          inset: 0;
          background: rgba(0, 0, 0, 0.3);
          animation: fade-in var(--duration-base) var(--easing-standard);
        }

        .center-panel {
          position: absolute;
          top: 0;
          right: 0;
          width: 400px;
          height: 100%;
          background: var(--system-background);
          box-shadow: var(--shadow-xl);
          display: flex;
          flex-direction: column;
          animation: notification-center-slide-in var(--duration-notification-slide) var(--easing-standard);
        }

        .center-panel.closing {
          animation: notification-center-slide-out var(--duration-notification-slide) var(--easing-standard);
        }

        .header {
          padding: var(--spacing-4);
          border-bottom: 1px solid var(--separator-opaque);
          display: flex;
          justify-content: space-between;
          align-items: center;
        }

        .header-title {
          font-family: var(--font-family-system);
          font-size: var(--font-size-lg);
          font-weight: var(--font-weight-semibold);
          color: var(--system-foreground);
          margin: 0;
        }

        .clear-all-btn {
          font-family: var(--font-family-system);
          font-size: var(--font-size-sm);
          color: var(--accent-color);
          background: transparent;
          border: none;
          padding: var(--spacing-1) var(--spacing-2);
          border-radius: var(--radius-sm);
          cursor: pointer;
          transition: background var(--duration-fast) var(--easing-standard);
        }

        .clear-all-btn:hover {
          background: var(--fill-tertiary);
        }

        .notifications-list {
          flex: 1;
          overflow-y: auto;
          padding: var(--spacing-2);
        }

        .notification-item {
          background: var(--system-background-secondary);
          border-radius: var(--radius-lg);
          padding: var(--spacing-3);
          margin-bottom: var(--spacing-2);
          display: flex;
          gap: var(--spacing-3);
          align-items: flex-start;
          position: relative;
        }

        .notification-icon {
          width: 40px;
          height: 40px;
          border-radius: var(--radius-md);
          object-fit: cover;
          flex-shrink: 0;
        }

        .notification-content {
          flex: 1;
          min-width: 0;
        }

        .notification-title {
          font-family: var(--font-family-system);
          font-size: var(--font-size-sm);
          font-weight: var(--font-weight-semibold);
          color: var(--system-foreground);
          margin: 0 0 var(--spacing-1);
        }

        .notification-message {
          font-family: var(--font-family-system);
          font-size: var(--font-size-xs);
          color: var(--system-foreground-secondary);
          margin: 0 0 var(--spacing-1);
          word-wrap: break-word;
        }

        .notification-timestamp {
          font-family: var(--font-family-system);
          font-size: var(--font-size-2xs);
          color: var(--system-foreground-tertiary);
        }

        .remove-btn {
          position: absolute;
          top: var(--spacing-2);
          right: var(--spacing-2);
          width: 20px;
          height: 20px;
          border-radius: var(--radius-full);
          background: transparent;
          border: none;
          cursor: pointer;
          display: flex;
          align-items: center;
          justify-content: center;
          color: var(--system-foreground-tertiary);
          transition: background var(--duration-fast) var(--easing-standard);
        }

        .remove-btn:hover {
          background: var(--fill-tertiary);
        }

        .remove-btn::before {
          content: '×';
          font-size: 16px;
          line-height: 1;
        }

        .empty-state {
          flex: 1;
          display: flex;
          flex-direction: column;
          align-items: center;
          justify-content: center;
          padding: var(--spacing-8);
          color: var(--system-foreground-tertiary);
        }

        .empty-state-icon {
          font-size: 48px;
          margin-bottom: var(--spacing-3);
        }

        .empty-state-text {
          font-family: var(--font-family-system);
          font-size: var(--font-size-base);
          text-align: center;
        }
      </style>

      <div class="overlay"></div>
      <div class="center-panel" role="dialog" aria-label="Notification Center">
        <div class="header">
          <h2 class="header-title">Notifications</h2>
          <button class="clear-all-btn">Clear All</button>
        </div>
        <div class="notifications-list"></div>
        <div class="empty-state" style="display: none;">
          <div class="empty-state-icon">🔔</div>
          <div class="empty-state-text">No notifications</div>
        </div>
      </div>
    `,this.shadowRoot.appendChild(e.content.cloneNode(!0))}_setupEventListeners(){const e=this.shadowRoot.querySelector(".overlay"),t=this.shadowRoot.querySelector(".clear-all-btn");this._overlayClickHandler=()=>this.close(),e.addEventListener("click",this._overlayClickHandler),this._clearAllHandler=()=>this.clearAll(),t.addEventListener("click",this._clearAllHandler),this._keydownHandler=i=>{i.key==="Escape"&&this.hasAttribute("open")&&this.close()},document.addEventListener("keydown",this._keydownHandler)}_cleanup(){document.removeEventListener("keydown",this._keydownHandler)}_handleOpenChange(e){const t=this.shadowRoot.querySelector(".center-panel");e?(t.classList.remove("closing"),this.dispatchEvent(new CustomEvent("notification-center-open",{bubbles:!0,composed:!0}))):(t.classList.add("closing"),this.dispatchEvent(new CustomEvent("notification-center-close",{bubbles:!0,composed:!0})))}_renderNotifications(){const e=this.shadowRoot.querySelector(".notifications-list"),t=this.shadowRoot.querySelector(".empty-state");if(this._notifications.length===0){e.style.display="none",t.style.display="flex";return}e.style.display="block",t.style.display="none",e.innerHTML="",this._notifications.forEach(i=>{const n=document.createElement("div");n.className="notification-item",n.dataset.notificationId=i.id;const o=new Date(i.timestamp),a=this._formatTimestamp(o);n.innerHTML=`
        ${i.iconUrl?`<img class="notification-icon" src="${i.iconUrl}" alt="" />`:""}
        <div class="notification-content">
          <h3 class="notification-title">${i.title||""}</h3>
          <p class="notification-message">${i.message||""}</p>
          <div class="notification-timestamp">${a}</div>
        </div>
        <button class="remove-btn" data-notification-id="${i.id}" aria-label="Remove notification"></button>
      `,n.querySelector(".remove-btn").addEventListener("click",l=>{l.stopPropagation(),this.removeNotification(i.id)}),e.appendChild(n)})}_formatTimestamp(e){const i=new Date-e,n=Math.floor(i/6e4);if(n<1)return"Just now";if(n<60)return`${n}m ago`;const o=Math.floor(n/60);if(o<24)return`${o}h ago`;const a=Math.floor(o/24);return a<7?`${a}d ago`:e.toLocaleDateString()}}customElements.define("notification-center",re);class de extends HTMLElement{static get observedAttributes(){return["open","active-panel"]}constructor(){super(),this.attachShadow({mode:"open"}),this._updatePromise=null,this._resolveUpdate=null}connectedCallback(){this._render(),this._setupEventListeners()}disconnectedCallback(){this._cleanup()}attributeChangedCallback(e,t,i){t!==i&&this._handleAttributeChange(e,i)}get updateComplete(){return this._updatePromise||(this._updatePromise=new Promise(e=>{this._resolveUpdate=e}),Promise.resolve().then(()=>{this._resolveUpdate&&(this._resolveUpdate(),this._updatePromise=null,this._resolveUpdate=null)})),this._updatePromise}open(){this.setAttribute("open","")}close(){if(this.hasAttribute("windowed")){const e=this.closest("likes-window");if(e?.close){e.close();return}}this.removeAttribute("open")}openPanel(e){this.setAttribute("active-panel",e),this.open()}_render(){const e=document.createElement("template");e.innerHTML=`
      <style>
        :host {
          display: none;
          position: fixed;
          inset: 0;
          z-index: var(--z-modal, 1400);
        }

        :host([open]),
        :host([windowed]) {
          display: block;
        }

        :host([windowed]) {
          position: static;
          inset: auto;
          z-index: auto;
          width: 100%;
          height: 100%;
        }

        :host([windowed]) .overlay {
          display: none;
        }

        :host([windowed]) .settings-modal {
          position: relative;
          top: auto;
          left: auto;
          transform: none;
          width: 100%;
          max-width: none;
          height: 100%;
          max-height: none;
          border-radius: 0;
          box-shadow: none;
          animation: none;
        }

        .overlay {
          position: absolute;
          inset: 0;
          background: rgba(0, 0, 0, 0.4);
          animation: fade-in var(--duration-base) var(--easing-standard);
        }

        .settings-modal {
          position: absolute;
          top: 50%;
          left: 50%;
          transform: translate(-50%, -50%);
          width: 800px;
          max-width: 90vw;
          height: 600px;
          max-height: 90vh;
          background: var(--system-background);
          border-radius: var(--radius-xl);
          box-shadow: var(--shadow-2xl);
          display: flex;
          overflow: hidden;
          animation: modal-slide-up var(--duration-medium) var(--easing-standard);
        }

        .settings-modal.open {
          animation: modal-slide-up var(--duration-medium) var(--easing-standard);
        }

        .sidebar {
          width: 240px;
          background: rgba(0, 0, 0, 0.15);
          border-right: 1px solid var(--separator-opaque);
          padding: var(--spacing-4);
          overflow-y: auto;
          display: flex;
          flex-direction: column;
          gap: var(--spacing-3);
        }

        .user-profile {
          display: flex;
          align-items: center;
          gap: var(--spacing-3);
          padding: var(--spacing-3);
          border-radius: var(--radius-md);
          background: rgba(255, 255, 255, 0.05);
        }

        .user-avatar {
          width: 48px;
          height: 48px;
          border-radius: 50%;
          background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
          display: flex;
          align-items: center;
          justify-content: center;
          font-size: var(--font-size-lg);
        }

        .user-info {
          flex: 1;
          display: flex;
          flex-direction: column;
        }

        .user-name {
          font-family: var(--font-family-system);
          font-size: var(--font-size-sm);
          font-weight: var(--font-weight-medium);
          color: var(--system-foreground);
        }

        .user-type {
          font-family: var(--font-family-system);
          font-size: var(--font-size-xs);
          color: var(--system-foreground-secondary);
        }

        .search-input {
          width: 100%;
          padding: var(--spacing-2) var(--spacing-3);
          border: none;
          border-radius: var(--radius-md);
          background: rgba(255, 255, 255, 0.08);
          color: var(--system-foreground);
          font-family: var(--font-family-system);
          font-size: var(--font-size-sm);
        }

        .search-input::placeholder {
          color: var(--system-foreground-tertiary);
        }

        .search-input:focus {
          outline: 2px solid var(--accent-color);
          outline-offset: 0;
          background: rgba(255, 255, 255, 0.12);
        }

        .panel-list {
          list-style: none;
          padding: 0;
          margin: 0;
          display: flex;
          flex-direction: column;
          gap: var(--spacing-1);
        }

        .panel-item {
          padding: var(--spacing-2) var(--spacing-3);
          border-radius: var(--radius-md);
          cursor: pointer;
          font-family: var(--font-family-system);
          font-size: var(--font-size-sm);
          color: var(--system-foreground);
          transition: all var(--duration-fast) var(--easing-standard);
          display: flex;
          align-items: center;
          gap: var(--spacing-3);
        }

        .panel-item:hover {
          background: rgba(255, 255, 255, 0.08);
        }

        .panel-item.active {
          background: var(--accent-color);
          color: white;
          font-weight: var(--font-weight-medium);
        }

        .panel-icon {
          width: 28px;
          height: 28px;
          border-radius: var(--radius-sm);
          display: flex;
          align-items: center;
          justify-content: center;
          font-size: 16px;
          flex-shrink: 0;
        }

        .panel-item.active .panel-icon {
          background: rgba(255, 255, 255, 0.2);
        }

        .content-area {
          flex: 1;
          display: flex;
          flex-direction: column;
          overflow: hidden;
        }

        .header {
          padding: var(--spacing-6) var(--spacing-6);
          border-bottom: 1px solid var(--separator-opaque);
          display: flex;
          justify-content: space-between;
          align-items: center;
          background: var(--system-background);
        }

        .header-title {
          font-family: var(--font-family-system);
          font-size: var(--font-size-2xl);
          font-weight: var(--font-weight-semibold);
          color: var(--system-foreground);
          margin: 0;
        }

        .close-btn {
          width: 32px;
          height: 32px;
          border-radius: var(--radius-full);
          background: transparent;
          border: none;
          cursor: pointer;
          display: flex;
          align-items: center;
          justify-content: center;
          color: var(--system-foreground-tertiary);
          transition: background var(--duration-fast) var(--easing-standard);
        }

        .close-btn:hover {
          background: var(--fill-tertiary);
        }

        .close-btn::before {
          content: '×';
          font-size: 28px;
          line-height: 1;
        }

        .panel-content {
          flex: 1;
          overflow-y: auto;
          padding: var(--spacing-6);
          background: var(--system-background);
        }

        .panel {
          display: none;
        }

        .panel.active {
          display: block;
        }
      </style>

      <div class="overlay"></div>
      <div class="settings-modal" role="dialog" aria-modal="true" aria-label="System Settings">
        <div class="sidebar">
          <div class="user-profile">
            <div class="user-avatar">👤</div>
            <div class="user-info">
              <div class="user-name">Guest User</div>
              <div class="user-type">Local Account</div>
            </div>
          </div>
          <input type="text" class="search-input" placeholder="Search" aria-label="Search settings" />
          <ul class="panel-list" role="list">
            <li class="panel-item" data-panel="desktop" role="button" tabindex="0">
              <span class="panel-icon">🖥️</span>
              <span>Desktop</span>
            </li>
            <li class="panel-item" data-panel="datetime" role="button" tabindex="0">
              <span class="panel-icon">🕐</span>
              <span>Date & Time</span>
            </li>
            <li class="panel-item" data-panel="notifications" role="button" tabindex="0">
              <span class="panel-icon">🔔</span>
              <span>Notifications</span>
            </li>
            <li class="panel-item" data-panel="appearance" role="button" tabindex="0">
              <span class="panel-icon">🎨</span>
              <span>Appearance</span>
            </li>
            <li class="panel-item" data-panel="weather" role="button" tabindex="0">
              <span class="panel-icon">☀️</span>
              <span>Weather</span>
            </li>
            <li class="panel-item" data-panel="pwa" role="button" tabindex="0">
              <span class="panel-icon">📦</span>
              <span>PWA</span>
            </li>
            <li class="panel-item" data-panel="agents" role="button" tabindex="0">
              <span class="panel-icon">🤖</span>
              <span>AI Agents</span>
            </li>
            <li class="panel-item" data-panel="aiua" role="button" tabindex="0">
              <span class="panel-icon">⚡</span>
              <span>Aiua Membrane</span>
            </li>
            <li class="panel-item" data-panel="credentials" role="button" tabindex="0">
              <span class="panel-icon">🔐</span>
              <span>Credentials</span>
            </li>
          </ul>
        </div>

        <div class="content-area">
          <div class="header">
            <h2 class="header-title">Settings</h2>
            <button class="close-btn" aria-label="Close settings"></button>
          </div>

          <div class="panel-content">
            <div class="panel" data-panel="desktop">
              <slot name="desktop-settings">
                <desktop-settings></desktop-settings>
              </slot>
            </div>
            <div class="panel" data-panel="datetime">
              <slot name="datetime-settings">
                <datetime-settings></datetime-settings>
              </slot>
            </div>
            <div class="panel" data-panel="notifications">
              <slot name="notification-settings">
                <notification-settings></notification-settings>
              </slot>
            </div>
            <div class="panel" data-panel="appearance">
              <slot name="appearance-settings">
                <appearance-settings></appearance-settings>
              </slot>
            </div>
            <div class="panel" data-panel="weather">
              <slot name="weather-settings">
                <weather-settings></weather-settings>
              </slot>
            </div>
            <div class="panel" data-panel="pwa">
              <slot name="pwa-settings">
                <pwa-settings></pwa-settings>
              </slot>
            </div>
            <div class="panel" data-panel="agents">
              <slot name="agent-settings">
                <agent-settings></agent-settings>
              </slot>
            </div>
            <div class="panel" data-panel="aiua">
              <slot name="aiua-membrane-settings">
                <aiua-membrane-settings></aiua-membrane-settings>
              </slot>
            </div>
            <div class="panel" data-panel="credentials">
              <slot name="agent-credentials-settings">
                <agent-credentials-settings></agent-credentials-settings>
              </slot>
            </div>
          </div>
        </div>
      </div>
    `,this.shadowRoot.appendChild(e.content.cloneNode(!0)),this._updateActivePanel()}_setupEventListeners(){const e=this.shadowRoot.querySelector(".overlay"),t=this.shadowRoot.querySelector(".close-btn"),i=this.shadowRoot.querySelectorAll(".panel-item"),n=this.shadowRoot.querySelector(".search-input");this._overlayClickHandler=()=>this.close(),e.addEventListener("click",this._overlayClickHandler),this._closeBtnHandler=()=>this.close(),t.addEventListener("click",this._closeBtnHandler),this._panelClickHandlers=[],i.forEach(o=>{const a=()=>{const d=o.dataset.panel;this.setAttribute("active-panel",d)};this._panelClickHandlers.push({item:o,handler:a}),o.addEventListener("click",a)}),this._searchHandler=o=>{const a=o.target.value.toLowerCase();i.forEach(d=>{const l=d.textContent.toLowerCase();d.style.display=l.includes(a)?"flex":"none"})},n.addEventListener("input",this._searchHandler),this._keydownHandler=o=>{o.key==="Escape"&&this.hasAttribute("open")&&this.close()},document.addEventListener("keydown",this._keydownHandler)}_cleanup(){document.removeEventListener("keydown",this._keydownHandler)}_handleAttributeChange(e,t){switch(e){case"open":this._handleOpenChange(t!==null);break;case"active-panel":this._updateActivePanel();break}}_handleOpenChange(e){const t=this.shadowRoot.querySelector(".settings-modal");e?(t.classList.add("open"),this.dispatchEvent(new CustomEvent("settings-open",{bubbles:!0,composed:!0}))):(t.classList.remove("open"),this.dispatchEvent(new CustomEvent("settings-close",{bubbles:!0,composed:!0})))}_updateActivePanel(){const e=this.getAttribute("active-panel")||"desktop",t=this.shadowRoot.querySelectorAll(".panel"),i=this.shadowRoot.querySelectorAll(".panel-item");t.forEach(n=>{n.classList.toggle("active",n.dataset.panel===e)}),i.forEach(n=>{n.classList.toggle("active",n.dataset.panel===e)}),this.dispatchEvent(new CustomEvent("panel-change",{detail:{panel:e},bubbles:!0,composed:!0}))}}customElements.define("system-settings",de);function le(){return`
    :host {
      display: block;
    }

    /* Standard setting group container */
    .setting-group {
      margin-bottom: var(--spacing-8);
    }

    /* Main section titles (h3) */
    .setting-group-title {
      font-family: var(--font-family-system);
      font-size: var(--font-size-lg);
      font-weight: var(--font-weight-semibold);
      color: var(--system-foreground);
      margin: 0 0 var(--spacing-4);
    }

    /* Optional description under title */
    .setting-group-description {
      font-family: var(--font-family-system);
      font-size: var(--font-size-sm);
      color: var(--system-foreground-secondary);
      margin: 0 0 var(--spacing-5);
      line-height: 1.6;
    }

    /* Individual setting row */
    .setting-item {
      display: flex;
      justify-content: space-between;
      align-items: center;
      padding: var(--spacing-4) var(--spacing-4);
      margin-bottom: var(--spacing-2);
      background: transparent;
      border-bottom: 1px solid var(--separator);
      border-radius: 0;
    }

    .setting-item:last-child {
      border-bottom: none;
    }

    /* Setting label */
    .setting-label {
      font-family: var(--font-family-system);
      font-size: var(--font-size-base);
      font-weight: var(--font-weight-medium);
      color: var(--system-foreground);
    }

    /* Setting description/hint */
    .setting-description {
      font-family: var(--font-family-system);
      font-size: var(--font-size-sm);
      color: var(--system-foreground-secondary);
      margin-top: var(--spacing-1);
      line-height: 1.4;
    }

    /* Standard buttons */
    button {
      padding: var(--spacing-2) var(--spacing-4);
      border: 1px solid var(--control-border);
      border-radius: var(--radius-input);
      background: var(--control-background);
      color: var(--system-foreground);
      font-family: var(--font-family-system);
      font-size: var(--font-size-sm);
      cursor: pointer;
      transition: background var(--duration-fast) var(--easing-standard);
    }

    button:hover {
      background: var(--control-background-hover);
    }

    button:active {
      background: var(--control-background-active);
    }

    button.primary {
      background: var(--accent-color);
      color: white;
      border-color: var(--accent-color);
    }

    button.primary:hover {
      opacity: 0.9;
    }

    button.danger {
      background: var(--accent-red);
      color: white;
      border-color: var(--accent-red);
    }

    button.danger:hover {
      opacity: 0.9;
    }

    button:disabled {
      opacity: 0.5;
      cursor: not-allowed;
    }

    /* Status indicators */
    .status-indicator {
      display: inline-flex;
      align-items: center;
      gap: var(--spacing-2);
      padding: var(--spacing-2) var(--spacing-3);
      background: var(--system-background-secondary);
      border-radius: var(--radius-md);
      font-family: var(--font-family-system);
      font-size: var(--font-size-xs);
      color: var(--system-foreground);
    }

    .status-dot {
      width: 8px;
      height: 8px;
      border-radius: 50%;
    }

    .status-dot.active {
      background: var(--accent-green);
    }

    .status-dot.inactive {
      background: var(--system-foreground-tertiary);
    }

    /* Button groups */
    .button-group {
      display: flex;
      gap: var(--spacing-2);
    }
  `}function ce(){if(typeof window<"u"){const r=window.location?.origin;if(r&&/^https?:\/\//.test(r))return r}return"http://localhost:7700"}class ue{constructor(){this._baseUrl=ce(),this._token=null,this._ws=null,this._wsReconnectTimer=null,this._isInitialized=!1,this._connected=!1}async initialize(){this._isInitialized||(await this._probeSession({quiet:!0}),this._isInitialized=!0)}async refreshSession(){return this._probeSession()}async connect(e,t){t&&(this._baseUrl=t,localStorage.setItem("aiua-base-url",t)),await this._applyToken(e,{validate:!0})}async disconnect(){this._token=null,this._connected=!1,this._closeWebSocket(),s.emit("aiua:disconnected")}isConnected(){return this._connected}getBaseUrl(){return this._baseUrl}async getStatus(){return this._get("/api/status")}async getGuests(){return this._get("/api/guests")}async getAgents(){return this._get("/api/agents")}async getAgent(e){const t=await this.getAgents(),i=Array.isArray(t)?t.find(n=>n.agent_id===e):null;if(!i)throw new Error(`Agent not found: ${e}`);return i}async getSessions(){return this._get("/api/sessions")}async getApartment(e){return this._get(`/api/apartments/${encodeURIComponent(e)}`)}async restartGuest(e){return this._post(`/api/guests/${encodeURIComponent(e)}/restart`)}async stopGuest(e){return this._post(`/api/guests/${encodeURIComponent(e)}/stop`)}async getComponents(){return this._get("/api/components")}async getComponentTemplates(){return this._get("/api/component-templates")}async getComponent(e){return this._get(`/api/components/${encodeURIComponent(e)}`)}async createComponent(e){return this._post("/api/components",e)}async updateComponent(e,t){return this._patch(`/api/components/${encodeURIComponent(e)}`,t)}async deleteComponent(e,t){return this._delete(`/api/components/${encodeURIComponent(e)}`,{confirm_guest_id:t})}async enableComponent(e){return this._post(`/api/components/${encodeURIComponent(e)}/enable`)}async disableComponent(e){return this._post(`/api/components/${encodeURIComponent(e)}/disable`)}async restartComponent(e){return this._post(`/api/components/${encodeURIComponent(e)}/restart`)}async getGraphInstances(){return this._get("/api/graphs")}async getSecrets(){return this._get("/api/secrets")}async updateAgent(e,t){return this._patch(`/api/agents/${encodeURIComponent(e)}`,t)}async getAgentRoles(e){return this._get(`/api/agents/${encodeURIComponent(e)}/roles`)}async getAgentRole(e,t){const i=await this.getAgentRoles(e),o=(Array.isArray(i)?i:i?.roles||[]).find(a=>a.role_name===t);if(!o)throw new Error(`Role not found: ${e}/${t}`);return o}async updateAgentRole(e,t,i){return this._patch(`/api/agents/${encodeURIComponent(e)}/roles/${encodeURIComponent(t)}`,i)}async getMeshTargets(){return this._get("/api/mesh/targets")}async createMeshInvite(e,t){const i={mesh_host:e};return t!=null&&t!==""&&(i.ttl_secs=t),this._post("/api/mesh/invite",i)}async getTargetStatus(e){return this._get(`/api/mesh/targets/${encodeURIComponent(e)}/status`)}async getTargetGuests(e){return this._get(`/api/mesh/targets/${encodeURIComponent(e)}/guests`)}async getTargetAgents(e){return this._get(`/api/mesh/targets/${encodeURIComponent(e)}/agents`)}async getTargetComponents(e){return this._get(`/api/mesh/targets/${encodeURIComponent(e)}/components`)}async getTargetComponent(e,t){return this._get(`/api/mesh/targets/${encodeURIComponent(e)}/components/${encodeURIComponent(t)}`)}async restartTargetComponent(e,t,i=t){return this._post(`/api/mesh/targets/${encodeURIComponent(e)}/components/${encodeURIComponent(t)}/restart`,{confirm_guest_id:i})}async enableTargetComponent(e,t){return this._post(`/api/mesh/targets/${encodeURIComponent(e)}/components/${encodeURIComponent(t)}/enable`)}async disableTargetComponent(e,t){return this._post(`/api/mesh/targets/${encodeURIComponent(e)}/components/${encodeURIComponent(t)}/disable`)}async getTargetConfig(e){return this._get(`/api/mesh/targets/${encodeURIComponent(e)}/config`)}async setTargetConfig(e,t,i){return this._put(`/api/mesh/targets/${encodeURIComponent(e)}/config/${encodeURIComponent(t)}`,{value:i})}async getTargetBestPlaceToRun(e,t={}){const i=new URLSearchParams;Object.entries(t).forEach(([o,a])=>{a==null||a===""||(Array.isArray(a)?a.forEach(d=>i.append(o,d)):i.set(o,String(a)))});const n=i.toString()?`?${i.toString()}`:"";return this._get(`/api/mesh/targets/${encodeURIComponent(e)}/best-place-to-run${n}`)}async getSkills(){return this._get("/api/skills")}async getToolsets(){return this._get("/api/toolsets")}async getConfig(){return this._get("/api/config")}async getTelegramConfig(){return this._get("/api/config/telegram")}async getGeminiConfig(){return this._get("/api/config/gemini")}async assignSkill(e,t,i){return this._post(`/api/agents/${encodeURIComponent(e)}/roles/${encodeURIComponent(t)}/skills`,{skill_name:i})}async chatWithAgent(e,t,i,n){return this._post(`/api/mesh/targets/${encodeURIComponent(e)}/agents/${encodeURIComponent(t)}/chat`,{content:i,conversation_id:n})}async getCronJobs(){return this._get("/api/cron")}async createCronJob(e){return this._post("/api/cron",e)}async deleteCronJob(e){return this._delete(`/api/cron/${encodeURIComponent(e)}`)}async enableCronJob(e){return this._post(`/api/cron/${encodeURIComponent(e)}/enable`)}async disableCronJob(e){return this._post(`/api/cron/${encodeURIComponent(e)}/disable`)}async getUserProfile(){return this._get("/api/user-profile")}async patchUserProfile(e){return this._patch("/api/user-profile",e)}async setConfig(e,t){return this._put(`/api/config/${encodeURIComponent(e)}`,{value:t})}async rotateSecret(e,t){return this._post("/api/secrets/rotate",{secret_ref:e,plaintext:t})}async addVaultEntry(e,t,i=[]){return this._post("/api/vault",{vault_name:e,plaintext:t,allowed_roles:i})}async revokeSkill(e,t,i){return this._delete(`/api/agents/${encodeURIComponent(e)}/roles/${encodeURIComponent(t)}/skills/${encodeURIComponent(i)}`)}openWebSocket(){if(this._ws||!this._connected)return;const e=this._baseUrl.replace(/^http/,"ws")+"/ws";try{this._ws=new WebSocket(e),this._ws.addEventListener("open",()=>{console.log("[AiuaService] WebSocket connected"),s.emit("aiua:ws-connected")}),this._ws.addEventListener("message",t=>{try{const i=JSON.parse(t.data);this._handleWsMessage(i)}catch{}}),this._ws.addEventListener("close",()=>{this._ws=null,s.emit("aiua:ws-disconnected"),this._connected&&(this._wsReconnectTimer=setTimeout(()=>this.openWebSocket(),5e3))}),this._ws.addEventListener("error",t=>{console.warn("[AiuaService] WebSocket error:",t)})}catch(t){console.error("[AiuaService] Failed to open WebSocket:",t)}}async _applyToken(e,{validate:t=!1,quiet:i=!1}={}){if(t){const n=await fetch(`${this._baseUrl}/api/status`,{credentials:"same-origin",headers:{Authorization:`Bearer ${e}`}});if(!n.ok)throw new Error(`Token rejected: ${n.status}`)}this._token=e,this._connected=!0,i||s.emit("aiua:connected"),this.openWebSocket()}async _probeSession({quiet:e=!1}={}){try{return(await fetch(`${this._baseUrl}/api/status`,{credentials:"same-origin",cache:"no-store"})).ok?(this._token="__cookie_session__",this._connected=!0,e||s.emit("aiua:connected"),this.openWebSocket(),!0):(this._token=null,this._connected=!1,!1)}catch{return this._token=null,this._connected=!1,!1}}_handleWsMessage(e){switch(e.type){case"guest:state":s.emit("aiua:guest-updated",e.payload);break;case"session:updated":s.emit("aiua:session-updated",e.payload);break;case"session:turn":s.emit("aiua:session-turn",e.payload);break;case"guest:started":s.emit("aiua:guest-started",e.payload);break;case"guest:stopped":s.emit("aiua:guest-stopped",e.payload);break;case"component:enabled":s.emit("aiua:component-enabled",e.payload);break;case"component:disabled":s.emit("aiua:component-disabled",e.payload);break;case"component:restarted":s.emit("aiua:component-restarted",e.payload);break;case"component:created":s.emit("aiua:component-created",e.payload);break;case"component:updated":s.emit("aiua:component-updated",e.payload);break;case"component:deleted":s.emit("aiua:component-deleted",e.payload);break;case"agent:updated":s.emit("aiua:agent-updated",e.payload);break;case"skill:assigned":s.emit("aiua:skill-assigned",e.payload);break;case"skill:revoked":s.emit("aiua:skill-revoked",e.payload);break;case"cron:created":s.emit("aiua:cron-created",e.payload);break;case"cron:updated":s.emit("aiua:cron-updated",e.payload);break;case"cron:deleted":s.emit("aiua:cron-deleted",e.payload);break;case"config:updated":s.emit("aiua:config-updated",e.payload);break;case"vault:entry-added":s.emit("aiua:vault-entry-added",e.payload);break;default:s.emit("aiua:ws-event",e)}}_closeWebSocket(){clearTimeout(this._wsReconnectTimer),this._ws&&(this._ws.close(),this._ws=null)}async _get(e){this._ensureConnected();const t=await fetch(`${this._baseUrl}${e}`,{credentials:"same-origin",headers:this._token&&this._token!=="__cookie_session__"?{Authorization:`Bearer ${this._token}`}:void 0});if(!t.ok)throw new Error(`GET ${e} failed: ${t.status}`);return t.json()}async _patch(e,t){this._ensureConnected();const i=await fetch(`${this._baseUrl}${e}`,{method:"PATCH",credentials:"same-origin",headers:{...this._token&&this._token!=="__cookie_session__"?{Authorization:`Bearer ${this._token}`}:{},"Content-Type":"application/json"},body:JSON.stringify(t)});if(!i.ok)throw new Error(`PATCH ${e} failed: ${i.status}`);return i.json()}async _delete(e,t){this._ensureConnected();const i=await fetch(`${this._baseUrl}${e}`,{method:"DELETE",credentials:"same-origin",headers:{...this._token&&this._token!=="__cookie_session__"?{Authorization:`Bearer ${this._token}`}:{},...t?{"Content-Type":"application/json"}:{}},body:t?JSON.stringify(t):void 0});if(!i.ok)throw new Error(`DELETE ${e} failed: ${i.status}`);return i.json()}async _put(e,t){this._ensureConnected();const i=await fetch(`${this._baseUrl}${e}`,{method:"PUT",credentials:"same-origin",headers:{...this._token&&this._token!=="__cookie_session__"?{Authorization:`Bearer ${this._token}`}:{},"Content-Type":"application/json"},body:t?JSON.stringify(t):void 0});if(!i.ok)throw new Error(`PUT ${e} failed: ${i.status}`);return i.json()}async _post(e,t){this._ensureConnected();const i=await fetch(`${this._baseUrl}${e}`,{method:"POST",credentials:"same-origin",headers:{...this._token&&this._token!=="__cookie_session__"?{Authorization:`Bearer ${this._token}`}:{},"Content-Type":"application/json"},body:t?JSON.stringify(t):void 0});if(!i.ok)throw new Error(`POST ${e} failed: ${i.status}`);return i.json()}_ensureConnected(){if(!this._connected)throw new Error("Not connected to aiua membrane")}}const I=new ue;class pe extends HTMLElement{constructor(){super(),this.attachShadow({mode:"open"}),this._error=null,this._message=null,this._busy=!1}connectedCallback(){this._render()}_currentBaseUrl(){try{return I.getBaseUrl()||localStorage.getItem("aiua-base-url")||window.location.origin||"http://127.0.0.1:7701"}catch{return"http://127.0.0.1:7701"}}async _retryCookieSession(){this._busy=!0,this._error=null,this._message=null,this._render();try{if(!await I.refreshSession())throw new Error("No same-origin membrane session was accepted by the server");s.emit("aiua:connected"),this._message="Same-origin membrane session restored."}catch(e){this._error=e.message||"Retry failed"}this._busy=!1,this._render()}async _submitToken(){const e=this.shadowRoot.querySelector("#aiua-url")?.value?.trim()||"http://127.0.0.1:7701",t=this.shadowRoot.querySelector("#aiua-token")?.value?.trim();if(!t){this._error="Session token is required",this._message=null,this._render();return}this._busy=!0,this._error=null,this._message=null,this._render();try{await I.connect(t,e),s.emit("aiua:connected"),this._message="Token accepted. Aiua membrane connected."}catch(i){this._error=i.message||"Token connect failed"}this._busy=!1,this._render()}_render(){const e=this._currentBaseUrl();this.shadowRoot.innerHTML=`
      <style>
        ${le()}
        .stack { display: grid; gap: 18px; }
        .card {
          padding: 16px;
          border-radius: 14px;
          background: rgba(255,255,255,0.04);
          border: 1px solid rgba(255,255,255,0.08);
        }
        .title {
          font-size: 18px;
          font-weight: 600;
          color: var(--system-foreground);
          margin: 0 0 8px;
          font-family: var(--font-family-system);
        }
        .copy {
          font-size: 13px;
          line-height: 1.6;
          color: var(--system-foreground-secondary);
          margin: 0;
          font-family: var(--font-family-system);
        }
        .field-group { display: grid; gap: 8px; margin-top: 16px; }
        .field-label {
          font-size: 12px;
          text-transform: uppercase;
          letter-spacing: 0.05em;
          color: var(--system-foreground-tertiary);
          font-family: var(--font-family-system);
        }
        .field-input {
          width: 100%;
          box-sizing: border-box;
          background: rgba(255,255,255,0.06);
          border: 1px solid rgba(255,255,255,0.12);
          border-radius: 10px;
          color: var(--system-foreground);
          font-size: 13px;
          padding: 10px 12px;
          outline: none;
          font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
        }
        .field-input:focus {
          border-color: var(--accent-color);
          background: rgba(255,255,255,0.08);
        }
        .button-row { display: flex; gap: 10px; flex-wrap: wrap; margin-top: 16px; }
        .status {
          margin-top: 14px;
          font-size: 12px;
          font-family: var(--font-family-system);
          line-height: 1.5;
        }
        .status.ok { color: #34c759; }
        .status.err { color: #ff453a; }
        .hint {
          margin-top: 10px;
          font-size: 11px;
          color: var(--system-foreground-tertiary);
          line-height: 1.6;
          font-family: var(--font-family-system);
        }
        code {
          background: rgba(255,255,255,0.08);
          border-radius: 4px;
          padding: 1px 5px;
        }
      </style>
      <div class="stack">
        <div class="card">
          <h3 class="title">Aiua Membrane</h3>
          <p class="copy">Use this panel when the desktop lost its same-origin membrane session but the hotel is still alive. The normal path should restore from cookie/session truth; the token path is a debug-grade fallback instead of a lifestyle.</p>
          <div class="button-row">
            <button class="primary" id="retry-cookie" ${this._busy?"disabled":""}>Retry Same-Origin Session</button>
          </div>
          <div class="hint">This asks the current server origin to re-establish the membrane session without needing a token.</div>
        </div>

        <div class="card">
          <h3 class="title">Fallback Token Connect</h3>
          <p class="copy">If the normal session is still unavailable, enter the debug token printed by <code>philotic-web serve</code>.</p>
          <div class="field-group">
            <label class="field-label" for="aiua-url">Server URL</label>
            <input class="field-input" id="aiua-url" value="${e}" />
          </div>
          <div class="field-group">
            <label class="field-label" for="aiua-token">Session Token</label>
            <input class="field-input" id="aiua-token" type="password" placeholder="philotic-…" />
          </div>
          <div class="button-row">
            <button class="primary" id="connect-token" ${this._busy?"disabled":""}>Connect With Token</button>
          </div>
          <div class="hint">The token is only the compatibility/debug path. Hotel-issued operator sessions should replace this eventually, because ambient bearer strings are efficient right up until they are the whole security model.</div>
          ${this._message?`<div class="status ok">${this._message}</div>`:""}
          ${this._error?`<div class="status err">${this._error}</div>`:""}
        </div>
      </div>
    `,this.shadowRoot.querySelector("#retry-cookie")?.addEventListener("click",()=>this._retryCookieSession()),this.shadowRoot.querySelector("#connect-token")?.addEventListener("click",()=>this._submitToken()),this.shadowRoot.querySelector("#aiua-token")?.addEventListener("keydown",t=>{t.key==="Enter"&&this._submitToken()})}}customElements.define("aiua-membrane-settings",pe);const he="likesos-filesystem",ge=1,f={TREE:"filesystem-tree",CONTENT:"filesystem-content",SYNC:"sync-metadata"};class me{constructor(){this.db=null,this.dbName=he,this.dbVersion=ge}async initialize(){return new Promise((e,t)=>{const i=indexedDB.open(this.dbName,this.dbVersion);i.onerror=()=>{t(new Error(`Failed to open IndexedDB: ${i.error}`))},i.onsuccess=()=>{this.db=i.result,e()},i.onupgradeneeded=n=>{const o=n.target.result;o.objectStoreNames.contains(f.TREE)||o.createObjectStore(f.TREE,{keyPath:"id"}),o.objectStoreNames.contains(f.CONTENT)||o.createObjectStore(f.CONTENT,{keyPath:"id"}),o.objectStoreNames.contains(f.SYNC)||o.createObjectStore(f.SYNC,{keyPath:"id"})}})}async saveTree(e,t=3){if(!this.db)throw new Error("Database not initialized. Call initialize() first.");return new Promise((i,n)=>{const a=this.db.transaction([f.TREE],"readwrite").objectStore(f.TREE),d={id:"root",version:t,tree:e,lastModified:Date.now()},l=a.put(d);l.onsuccess=()=>i(),l.onerror=()=>n(new Error(`Failed to save tree: ${l.error}`))})}async loadTree(){if(!this.db)throw new Error("Database not initialized. Call initialize() first.");return new Promise((e,t)=>{const o=this.db.transaction([f.TREE],"readonly").objectStore(f.TREE).get("root");o.onsuccess=()=>{e(o.result||null)},o.onerror=()=>t(new Error(`Failed to load tree: ${o.error}`))})}async saveContent(e,t,i={}){if(!this.db)throw new Error("Database not initialized. Call initialize() first.");return new Promise((n,o)=>{const d=this.db.transaction([f.CONTENT],"readwrite").objectStore(f.CONTENT),l={id:e,content:t,size:i.size||(typeof t=="string"?t.length:t.size),mimeType:i.mimeType||"text/plain",lastModified:Date.now()},c=d.put(l);c.onsuccess=()=>n(),c.onerror=()=>o(new Error(`Failed to save content: ${c.error}`))})}async loadContent(e){if(!this.db)throw new Error("Database not initialized. Call initialize() first.");return new Promise((t,i)=>{const a=this.db.transaction([f.CONTENT],"readonly").objectStore(f.CONTENT).get(e);a.onsuccess=()=>{t(a.result||null)},a.onerror=()=>i(new Error(`Failed to load content: ${a.error}`))})}async saveMultipleContents(e){if(!this.db)throw new Error("Database not initialized. Call initialize() first.");return new Promise((t,i)=>{const n=this.db.transaction([f.CONTENT],"readwrite"),o=n.objectStore(f.CONTENT);for(const a of e){const d={id:a.id,content:a.content,size:a.metadata?.size||(typeof a.content=="string"?a.content.length:a.content.size),mimeType:a.metadata?.mimeType||"text/plain",lastModified:Date.now()};o.put(d)}n.oncomplete=()=>t(),n.onerror=()=>i(new Error(`Failed to save multiple contents: ${n.error}`))})}async loadMultipleContents(e){if(!this.db)throw new Error("Database not initialized. Call initialize() first.");return new Promise((t,i)=>{const o=this.db.transaction([f.CONTENT],"readonly").objectStore(f.CONTENT),a=[];let d=0;const l=()=>{if(d>=e.length){t(a);return}const c=o.get(e[d]);c.onsuccess=()=>{a.push(c.result||null),d++,l()},c.onerror=()=>i(new Error(`Failed to load content: ${c.error}`))};l()})}async deleteContent(e){if(!this.db)throw new Error("Database not initialized. Call initialize() first.");return new Promise((t,i)=>{const a=this.db.transaction([f.CONTENT],"readwrite").objectStore(f.CONTENT).delete(e);a.onsuccess=()=>t(),a.onerror=()=>i(new Error(`Failed to delete content: ${a.error}`))})}async deleteMultipleContents(e){if(!this.db)throw new Error("Database not initialized. Call initialize() first.");return new Promise((t,i)=>{const n=this.db.transaction([f.CONTENT],"readwrite"),o=n.objectStore(f.CONTENT);for(const a of e)o.delete(a);n.oncomplete=()=>t(),n.onerror=()=>i(new Error(`Failed to delete multiple contents: ${n.error}`))})}async getSyncState(){if(!this.db)throw new Error("Database not initialized. Call initialize() first.");return new Promise((e,t)=>{const o=this.db.transaction([f.SYNC],"readonly").objectStore(f.SYNC).get("sync-state");o.onsuccess=()=>{e(o.result||{id:"sync-state",lastSync:null,pendingChanges:[],syncEnabled:!1})},o.onerror=()=>t(new Error(`Failed to load sync state: ${o.error}`))})}async updateSyncState(e){if(!this.db)throw new Error("Database not initialized. Call initialize() first.");return new Promise((t,i)=>{const o=this.db.transaction([f.SYNC],"readwrite").objectStore(f.SYNC),a={id:"sync-state",lastSync:e.lastSync||null,pendingChanges:e.pendingChanges||[],syncEnabled:e.syncEnabled||!1,updatedAt:Date.now()},d=o.put(a);d.onsuccess=()=>t(),d.onerror=()=>i(new Error(`Failed to update sync state: ${d.error}`))})}close(){this.db&&(this.db.close(),this.db=null)}async deleteDatabase(){return this.close(),new Promise((e,t)=>{const i=indexedDB.deleteDatabase(this.dbName);i.onsuccess=()=>e(),i.onerror=()=>t(new Error(`Failed to delete database: ${i.error}`)),i.onblocked=()=>{console.warn("Database deletion blocked. Close all connections first."),setTimeout(e,100)}})}}async function fe(){const r=localStorage.getItem("filesystem-root")!==null;if(!r)return!1;try{const e=await we(),n=e.transaction(["filesystem-tree"],"readonly").objectStore("filesystem-tree").get("root"),o=await new Promise(a=>{n.onsuccess=()=>a(n.result!==void 0),n.onerror=()=>a(!1)});return e.close(),r&&!o}catch{return!0}}async function we(){return new Promise((r,e)=>{const t=indexedDB.open("likesos-filesystem",1);t.onsuccess=()=>r(t.result),t.onerror=()=>e(t.error)})}function be(){try{const r=localStorage.getItem("filesystem-root");if(!r)return null;const e=JSON.parse(r);if(!e.root||!e.version)throw new Error("Invalid localStorage structure");return e}catch(r){return console.error("Failed to extract from localStorage:",r),null}}function ve(r){const e=new Map;function t(n){const o={...n};return n.type==="file"&&n.content!==void 0&&(e.set(n.id,{id:n.id,content:n.content,size:n.size||0,mimeType:n.mimeType||"text/plain"}),delete o.content),n.children&&(o.children=n.children.map(a=>t(a))),o}return{tree:t(r),contents:Array.from(e.values())}}function ye(r,e){function t(i,n){return!i||!n||i.id!==n.id||i.name!==n.name||i.type!==n.type?!1:i.children&&n.children?i.children.length!==n.children.length?!1:i.children.every((o,a)=>t(o,n.children[a])):!0}return t(r.root,e.tree)}async function _e(r){console.log("[Migration] Starting localStorage to IndexedDB migration...");try{if(!await fe())return console.log("[Migration] No migration needed"),{success:!0,skipped:!0};const t=be();if(!t)throw new Error("Failed to extract localStorage data");console.log("[Migration] Extracted data from localStorage (version "+t.version+")");const{tree:i,contents:n}=ve(t.root);if(console.log("[Migration] Separated tree and content:",{treeNodeCount:X(i),contentCount:n.length}),await r.initialize(),await r.saveTree(i,t.version),console.log("[Migration] Saved tree structure"),n.length>0){const l=n.map(c=>({id:c.id,content:c.content,metadata:{size:c.size,mimeType:c.mimeType}}));await r.saveMultipleContents(l),console.log("[Migration] Saved "+n.length+" file contents")}const o=await r.loadTree();if(!ye(t,o))throw new Error("Migration validation failed - data mismatch");console.log("[Migration] Validation successful");const d=localStorage.getItem("filesystem-root");return localStorage.setItem("filesystem-root-backup",d),localStorage.removeItem("filesystem-root"),console.log("[Migration] Cleared localStorage (backup saved)"),console.log("[Migration] Migration completed successfully!"),{success:!0}}catch(e){console.error("[Migration] Migration failed:",e);const t=localStorage.getItem("filesystem-root-backup");return t&&!localStorage.getItem("filesystem-root")&&(localStorage.setItem("filesystem-root",t),console.log("[Migration] Rolled back to localStorage")),{success:!1,error:e.message}}}function X(r){if(!r)return 0;let e=1;return r.children&&(e+=r.children.reduce((t,i)=>t+X(i),0)),e}class ke{constructor(){this._root=null,this._currentPath="/Users/Guest/Desktop",this._initialized=!1,this._version=3,this._indexedDB=new me,this._contentCache=new Map}async initialize(){if(this._initialized)return;console.log("[FileSystem] Initializing..."),await this._indexedDB.initialize();try{const t=await _e(this._indexedDB);t.success&&!t.skipped&&(console.log("[FileSystem] Successfully migrated from localStorage to IndexedDB"),s.emit("filesystem:migrated"))}catch(t){console.error("[FileSystem] Migration failed, continuing with IndexedDB:",t)}const e=await this._loadFromStorage();e&&e.version===this._version?(this._root=e.tree,await this._reattachContents(this._root),console.log("[FileSystem] Loaded existing filesystem (version "+this._version+")")):(console.log("[FileSystem] Creating new file system structure (version "+this._version+")"),this._root=this._createDefaultStructure(),await this._saveToStorage()),this._initialized=!0,s.emit("filesystem:initialized"),console.log("[FileSystem] Initialization complete")}getNode(e){const t=this._normalizePath(e);return this._findNode(t)}listDirectory(e){const t=this.getNode(e);if(!t)throw new Error(`Path not found: ${e}`);if(t.type!=="folder")throw new Error(`Not a directory: ${e}`);return t.children||[]}async createFolder(e,t){const i=this.getNode(e);if(!i)throw new Error(`Parent path not found: ${e}`);if(i.type!=="folder")throw new Error(`Parent is not a directory: ${e}`);if(i.children.some(o=>o.name===t))throw new Error(`Folder already exists: ${t}`);const n={id:this._generateId(),name:t,type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[]};return i.children.push(n),i.modified=new Date().toISOString(),await this._saveToStorage(),s.emit("filesystem:folder-created",{path:e,folder:n}),n}async createFile(e,t,i="",n="text/plain"){const o=this.getNode(e);if(!o)throw new Error(`Parent path not found: ${e}`);if(o.type!=="folder")throw new Error(`Parent is not a directory: ${e}`);if(o.children.some(d=>d.name===t))throw new Error(`File already exists: ${t}`);const a={id:this._generateId(),name:t,type:"file",mimeType:n,content:i,size:new Blob([i]).size,created:new Date().toISOString(),modified:new Date().toISOString()};return o.children.push(a),o.modified=new Date().toISOString(),await this._saveToStorage(),s.emit("filesystem:file-created",{path:e,file:a}),a}async updateFile(e,t,i=null){const n=this.getNode(e);if(!n)throw new Error(`File not found: ${e}`);if(n.type!=="file")throw new Error(`Not a file: ${e}`);return n.content=t,n.size=new Blob([t]).size,n.modified=new Date().toISOString(),i&&(n.mimeType=i),await this._saveToStorage(),s.emit("filesystem:file-updated",{path:e,file:n}),n}async delete(e){const t=this._normalizePath(e),i=this._getParentPath(t),n=this.getNode(i),o=this._getFileName(t);if(!n)throw new Error(`Parent path not found: ${i}`);const a=n.children.findIndex(l=>l.name===o);if(a===-1)throw new Error(`Not found: ${e}`);const d=n.children.splice(a,1)[0];n.modified=new Date().toISOString(),await this._saveToStorage(),s.emit("filesystem:deleted",{path:e,node:d})}async rename(e,t){const i=this.getNode(e);if(!i)throw new Error(`Path not found: ${e}`);const n=i.name;i.name=t,i.modified=new Date().toISOString(),await this._saveToStorage(),s.emit("filesystem:renamed",{path:e,oldName:n,newName:t})}async move(e,t){const i=this.getNode(e),n=this.getNode(t);if(!i)throw new Error(`Source not found: ${e}`);if(!n||n.type!=="folder")throw new Error(`Destination is not a directory: ${t}`);const o=this._getParentPath(e),a=this.getNode(o),d=a.children.findIndex(l=>l.id===i.id);a.children.splice(d,1),n.children.push(i),n.modified=new Date().toISOString(),await this._saveToStorage(),s.emit("filesystem:moved",{sourcePath:e,destPath:t})}async copy(e,t){const i=this.getNode(e),n=this.getNode(t);if(!i)throw new Error(`Source not found: ${e}`);if(!n||n.type!=="folder")throw new Error(`Destination is not a directory: ${t}`);const o=this._cloneNode(i);n.children.push(o),n.modified=new Date().toISOString(),await this._saveToStorage(),s.emit("filesystem:copied",{sourcePath:e,destPath:t})}search(e,t="/"){const i=[],n=e.toLowerCase(),o=(d,l)=>{d.name.toLowerCase().includes(n)&&i.push({path:l,node:{...d,children:void 0}}),d.type==="folder"&&d.children&&d.children.forEach(c=>{const p=l==="/"?`/${c.name}`:`${l}/${c.name}`;o(c,p)})},a=this.getNode(t);return a&&o(a,t),i}_createDefaultStructure(){return{id:"root",name:"",type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[{id:this._generateId(),name:"Users",type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[{id:this._generateId(),name:"Guest",type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[{id:this._generateId(),name:"Desktop",type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[{id:this._generateId(),name:"Welcome.txt",type:"file",mimeType:"text/plain",content:`Welcome to jaredlikes Desktop!

This is a web-based macOS Tahoe 26.1 environment.`,size:85,created:new Date().toISOString(),modified:new Date().toISOString()}]},{id:this._generateId(),name:"Documents",type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[]},{id:this._generateId(),name:"Downloads",type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[]},{id:this._generateId(),name:"Pictures",type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[]},{id:this._generateId(),name:"Music",type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[]}]}]},{id:this._generateId(),name:"Applications",type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[{id:this._generateId(),name:"Finder.app",type:"application",appId:"finder",icon:"📁",created:new Date().toISOString(),modified:new Date().toISOString()},{id:this._generateId(),name:"Safari.app",type:"application",appId:"safari",icon:"🧭",created:new Date().toISOString(),modified:new Date().toISOString()},{id:this._generateId(),name:"Mail.app",type:"application",appId:"mail",icon:"✉️",created:new Date().toISOString(),modified:new Date().toISOString()},{id:this._generateId(),name:"Messages.app",type:"application",appId:"messages",icon:"💬",created:new Date().toISOString(),modified:new Date().toISOString()},{id:this._generateId(),name:"Music.app",type:"application",appId:"music",icon:"🎵",created:new Date().toISOString(),modified:new Date().toISOString()},{id:this._generateId(),name:"Activity Monitor.app",type:"application",appId:"activity-monitor",icon:"📊",created:new Date().toISOString(),modified:new Date().toISOString()},{id:this._generateId(),name:"System Settings.app",type:"application",appId:"system-settings",icon:"⚙️",created:new Date().toISOString(),modified:new Date().toISOString()}]},{id:this._generateId(),name:"System",type:"folder",created:new Date().toISOString(),modified:new Date().toISOString(),children:[]}]}}_findNode(e){if(e==="/")return this._root;const t=e.split("/").filter(n=>n);let i=this._root;for(const n of t)if(!i.children||(i=i.children.find(o=>o.name===n),!i))return null;return i}_normalizePath(e){return e.replace(/\/+/g,"/").replace(/\/$/,"")||"/"}_getParentPath(e){const t=this._normalizePath(e),i=t.lastIndexOf("/");return i===0?"/":t.substring(0,i)}_getFileName(e){const t=this._normalizePath(e),i=t.lastIndexOf("/");return t.substring(i+1)}_generateId(){return`node_${Date.now()}_${Math.random().toString(36).substr(2,9)}`}_cloneNode(e){const t={...e,id:this._generateId()};return e.children&&(t.children=e.children.map(i=>this._cloneNode(i))),t}async _saveToStorage(){try{const{tree:e,contents:t}=this._separateContentFromTree(this._root);if(await this._indexedDB.saveTree(e,this._version),t.length>0){const i=t.map(n=>({id:n.id,content:n.content,metadata:{size:n.size,mimeType:n.mimeType}}));await this._indexedDB.saveMultipleContents(i)}t.forEach(i=>{this._contentCache.set(i.id,i.content)})}catch(e){throw console.error("[FileSystem] Failed to save file system:",e),e}}async _loadFromStorage(){try{return await this._indexedDB.loadTree()}catch(e){return console.error("[FileSystem] Failed to load file system:",e),null}}async _reattachContents(e){const t=[];function i(n){n.type==="file"&&t.push(n.id),n.children&&n.children.forEach(o=>i(o))}if(i(e),t.length>0){let a=function(d){if(d.type==="file"){const l=o.get(d.id);l!==void 0&&(d.content=l)}d.children&&d.children.forEach(l=>a(l))};const n=await this._indexedDB.loadMultipleContents(t),o=new Map;n.forEach((d,l)=>{d&&(o.set(t[l],d.content),this._contentCache.set(t[l],d.content))}),a(e)}}_separateContentFromTree(e){const t=[];function i(o){const a={...o};return o.type==="file"&&o.content!==void 0&&(t.push({id:o.id,content:o.content,size:o.size||0,mimeType:o.mimeType||"text/plain"}),delete a.content),o.children&&(a.children=o.children.map(d=>i(d))),a}return{tree:i(e),contents:t}}}const g=new ke;class xe{constructor(){this._preferences=null,this._listeners=new Map,this._cloudSyncEnabled=!1,this._preferencesPath="/Users/Guest/Library/Preferences",this._preferencesFile="com.jaredlikes.desktop.json",this._initialized=!1}async load(){try{await this._ensureInitialized();const e=g.getNode(`${this._preferencesPath}/${this._preferencesFile}`);return e&&e.content?(this._preferences=JSON.parse(e.content),console.log("[PreferencesService] Loaded preferences from filesystem")):(await this._migrateFromLocalStorage(),this._preferences||(this._preferences=this._getDefaults(),await this._saveToFile())),this._cloudSyncEnabled&&await this._syncFromCloud(),this._preferences}catch(e){return console.error("[PreferencesService] Failed to load preferences:",e),this._preferences=this._getDefaults(),this._preferences}}async save(e){this._preferences={...this._preferences,...e,lastModified:new Date().toISOString()};try{await this._saveToFile(),this._cloudSyncEnabled&&await this._syncToCloud(),this._notifyListeners(e)}catch(t){console.error("[PreferencesService] Failed to save preferences:",t)}}get(e){if(!this._preferences)return;const t=e.split(".");let i=this._preferences;for(const n of t)if(i&&typeof i=="object"&&n in i)i=i[n];else return;return i}async set(e,t){this._preferences||(this._preferences=this._getDefaults());const i=e.split("."),n=i.pop();let o=this._preferences;for(const d of i)(!o[d]||typeof o[d]!="object")&&(o[d]={}),o=o[d];const a=o[n];o[n]=t,await this.save(this._preferences),this._notifyListeners({[e]:t},a)}async setCloudSync(e){this._cloudSyncEnabled=e,e&&await this._syncNow()}async syncNow(){this._cloudSyncEnabled&&(await this._syncFromCloud(),await this._syncToCloud())}onChange(e){const t=Date.now()+Math.random();return this._listeners.set(t,e),()=>{this._listeners.delete(t)}}_getDefaults(){return{desktop:{backgroundImage:"gradient",backgroundColor:window.matchMedia("(prefers-color-scheme: dark)").matches?"#1e1e1e":"#f5f5f7"},dock:{position:"bottom",size:64,magnification:{enabled:!0,maxSize:128},autoHide:!1},notifications:{autoDismissDuration:5,doNotDisturb:!1,permissions:{},displayStyle:{},soundEnabled:{},badgeEnabled:{}},appearance:{theme:"auto",accentColor:"blue",transparency:!0,reducedMotion:!1},cloudSync:{enabled:!1,lastSyncTimestamp:null},lastModified:new Date().toISOString()}}_notifyListeners(e){this._listeners.forEach(t=>{t(e)})}async _syncFromCloud(){console.log("[PreferencesService] Cloud sync from cloud (not implemented)")}async _syncToCloud(){console.log("[PreferencesService] Cloud sync to cloud (not implemented)")}async _ensureInitialized(){if(this._initialized)return;this._initialized=!0,await g.initialize(),g.getNode("/Users/Guest/Library")||await g.createFolder("/Users/Guest","Library"),g.getNode(this._preferencesPath)||await g.createFolder("/Users/Guest/Library","Preferences")}async _saveToFile(){await this._ensureInitialized();const e=JSON.stringify(this._preferences,null,2),t=`${this._preferencesPath}/${this._preferencesFile}`;g.getNode(t)?await g.updateFile(t,e,"application/json"):await g.createFile(this._preferencesPath,this._preferencesFile,e,"application/json")}async _migrateFromLocalStorage(){try{const e=localStorage.getItem("macos-desktop-preferences");e&&(this._preferences=JSON.parse(e),await this._saveToFile(),localStorage.removeItem("macos-desktop-preferences"),console.log("[PreferencesService] Migrated preferences from localStorage"))}catch(e){console.warn("[PreferencesService] Failed to migrate from localStorage:",e)}}}const Se=new xe;class Ee{constructor(){this._applications=new Map,this._runningApps=new Map}register(e){if(!e.id)throw new Error("Application id is required");if(!e.name)throw new Error("Application name is required");if(this._applications.has(e.id))throw new Error(`Application "${e.id}" is already registered`);this._applications.set(e.id,{...e,iconUrl:e.iconUrl||"",onLaunch:e.onLaunch,onQuit:e.onQuit,permissions:e.permissions||{},registeredAt:new Date().toISOString()}),s.emit("app:registered",{appId:e.id})}unregister(e){if(this.isRunning(e))throw new Error("Cannot unregister running application");this._applications.delete(e),s.emit("app:unregistered",{appId:e})}launch(e,t={}){const i=this._applications.get(e);if(!i)throw new Error("Application not registered");const n=this.isRunning(e);if(!n){const o={appId:e,launchedAt:new Date().toISOString(),windows:[],options:t};this._runningApps.set(e,o),s.emit("app:launched",{appId:e,options:t})}if(i.onLaunch)try{const o=i.onLaunch(t);o&&typeof o.catch=="function"&&o.catch(a=>{console.error(`Error launching app "${e}":`,a)})}catch(o){throw console.error(`Error launching app "${e}":`,o),n||this._runningApps.delete(e),o}return!0}quit(e){const t=this._applications.get(e);if(!this._runningApps.get(e))throw new Error("Application not running");if(t?.onQuit)try{const n=t.onQuit();n&&typeof n.catch=="function"&&n.catch(o=>{console.error(`Error quitting app "${e}":`,o)})}catch(n){console.error(`Error quitting app "${e}":`,n)}this._runningApps.delete(e),s.emit("app:quit",{appId:e})}focus(e){this.isRunning(e)&&s.emit("app:focus",{appId:e})}isRunning(e){return this._runningApps.has(e)}getApp(e){const t=this._applications.get(e);return t?{...t}:null}getApplication(e){return this.getApp(e)}getAllApps(){return Array.from(this._applications.values())}getAllApplications(){return this.getAllApps()}getRunningApps(){return Array.from(this._runningApps.keys()).map(e=>({appId:e,...this._applications.get(e),...this._runningApps.get(e)}))}registerWindow(e,t){const i=this._runningApps.get(e);i&&!i.windows.includes(t)&&(i.windows.push(t),s.emit("app:window-registered",{appId:e,windowId:t}))}unregisterWindow(e,t){const i=this._runningApps.get(e);i&&(i.windows=i.windows.filter(n=>n!==t),s.emit("app:window-unregistered",{appId:e,windowId:t}),i.windows.length===0&&this._applications.get(e)?.terminateWhenNoWindows!==!1&&this.quit(e))}getAppWindows(e){const t=this._runningApps.get(e);return t?[...t.windows]:[]}}const b=new Ee;class Ae{constructor(){this._applications=new Map,this._activationHistory=[],this._setupEventListeners()}registerApplication(e){const t=e.id;if(this._applications.has(t))return console.warn(`Application ${t} already registered`),t;const i={manifest:e,component:null,windows:new Set,state:"inactive",launchedAt:null,activatedAt:null};return this._applications.set(t,i),s.emit("app:registered",{appId:t,manifest:e}),t}launchApplication(e,t){const i=this._applications.get(e);if(!i){console.error(`Application ${e} not registered`);return}i.component=t,i.launchedAt=Date.now(),i.state="active",s.emit("app:launched",{appId:e,manifest:i.manifest,component:t})}activateApplication(e){const t=this._applications.get(e);if(!t){console.error(`Application ${e} not registered`);return}this._applications.forEach((i,n)=>{n!==e&&i.state==="active"&&(i.state="background")}),t.state="active",t.activatedAt=Date.now(),this._activationHistory=this._activationHistory.filter(i=>i!==e),this._activationHistory.push(e),s.emit("app:activated",{appId:e,manifest:t.manifest})}terminateApplication(e){const t=this._applications.get(e);if(!t)return!1;if(t.manifest.canQuit===!1)return s.emit("app:quit-prevented",{appId:e,reason:"Application cannot be quit"}),!1;Array.from(t.windows).forEach(o=>{s.emit("window:close-requested",{windowId:o,appId:e})}),t.state="inactive",t.windows.clear(),this._activationHistory=this._activationHistory.filter(o=>o!==e);const n=this._getPreviousApplication();return n&&this.activateApplication(n),s.emit("app:terminated",{appId:e}),!0}_getPreviousApplication(){for(let e=this._activationHistory.length-1;e>=0;e--){const t=this._activationHistory[e],i=this._applications.get(t);if(i&&i.state!=="inactive"&&(t==="desktop"||i.windows.size>0))return t}return"desktop"}trackWindow(e,t){const i=this._applications.get(e);if(!i){console.error(`Application ${e} not registered`);return}i.windows.add(t),s.emit("app:window-added",{appId:e,windowId:t})}untrackWindow(e,t){const i=this._applications.get(e);i&&(i.windows.delete(t),s.emit("app:window-removed",{appId:e,windowId:t}),i.windows.size===0&&i.manifest.terminateWhenNoWindows&&this.terminateApplication(e))}getApplicationState(e){return this._applications.get(e)||null}getRunningApplications(){const e=[];return this._applications.forEach((t,i)=>{t.state!=="inactive"&&e.push({appId:i,manifest:t.manifest,state:t.state,windowCount:t.windows.size})}),e}getActiveApplication(){for(const[e,t]of this._applications.entries())if(t.state==="active")return{appId:e,manifest:t.manifest,component:t.component};return null}isApplicationRunning(e){const t=this._applications.get(e);return t?t.state!=="inactive":!1}getApplicationWindows(e){const t=this._applications.get(e);return t?new Set(t.windows):new Set}_setupEventListeners(){s.on("window:created",({windowId:e,appId:t})=>{this.trackWindow(t,e)}),s.on("window:closed",({windowId:e,appId:t})=>{this.untrackWindow(t,e)}),s.on("window:focused",({windowId:e,appId:t})=>{this.activateApplication(t)}),s.on("app:ready",({appId:e,component:t})=>{const i=this._applications.get(e);i&&!i.component&&this.launchApplication(e,t)})}}const C=new Ae;class Ce{constructor(){this._queue=[],this._active=[],this._history=[],this._dndEnabled=!1,this._maxActive=5,this._defaultDuration=5e3}show(e){const t=e.id||`notif-${Date.now()}-${Math.random()}`,i={id:t,title:e.title||"",message:e.message||"",iconUrl:e.iconUrl,actionLabel:e.actionLabel,duration:e.duration??this._defaultDuration,critical:e.critical||!1,timestamp:new Date().toISOString()};return this._dndEnabled&&!i.critical?(this._history.unshift(i),s.emit("notification:suppressed",i),t):(this._history.unshift(i),this._active.length<this._maxActive?this._displayNotification(i):this._queue.push(i),t)}dismiss(e){const t=this._active.findIndex(n=>n.id===e);t!==-1&&(this._active.splice(t,1),s.emit("notification:dismissed",{id:e}),this._processQueue());const i=this._queue.findIndex(n=>n.id===e);i!==-1&&this._queue.splice(i,1)}dismissAll(){this._active.forEach(e=>{s.emit("notification:dismissed",{id:e.id})}),this._active=[],this._queue=[]}setDoNotDisturb(e){this._dndEnabled=e,s.emit("notification:dnd-changed",{enabled:e}),e&&(this._active=this._active.filter(t=>t.critical?!0:(s.emit("notification:dismissed",{id:t.id}),!1)))}isDoNotDisturb(){return this._dndEnabled}getHistory(e){return e?this._history.slice(0,e):[...this._history]}clearHistory(){this._history=[],s.emit("notification:history-cleared")}removeFromHistory(e){this._history=this._history.filter(t=>t.id!==e),s.emit("notification:history-updated")}setMaxActive(e){this._maxActive=e}setDefaultDuration(e){this._defaultDuration=e}_displayNotification(e){this._active.push(e),s.emit("notification:show",e),e.duration>0&&setTimeout(()=>{this.dismiss(e.id)},e.duration)}_processQueue(){for(;this._active.length<this._maxActive&&this._queue.length>0;){const e=this._queue.shift();this._displayNotification(e)}}}const M=new Ce;class Ie{constructor(){this._windows=new Map,this._nextZIndex=100,this._focusedWindow=null,this._setupDesktopIntegration()}_setupDesktopIntegration(){s.on("desktop:switched",({from:e,to:t})=>{this._handleDesktopSwitch(e,t)})}_handleDesktopSwitch(e,t){w(async()=>{const{default:i}=await Promise.resolve().then(()=>W);return{default:i}},void 0).then(({default:i})=>{const n=i.getDesktop(e),o=i.getDesktop(t);n&&n.windows.forEach(a=>{const d=this._windows.get(a);d&&(d.element.style.display="none")}),o&&o.windows.forEach(a=>{const d=this._windows.get(a);d&&(d.element.style.display="")})})}createWindow(e={}){const{appId:t,title:i="Untitled",x:n=100,y:o=100,width:a=600,height:d=400,content:l="",appComponent:c=null}=e,p=`window-${t}-${Date.now()}`,u=document.createElement("likes-window");u.id=p,u.setAttribute("title",i),u.setAttribute("x",n),u.setAttribute("y",o),u.setAttribute("width",a),u.setAttribute("height",d),u.setAttribute("z-index",this._nextZIndex++),u.setAttribute("focused","true"),l&&(u.innerHTML=l),this._windows.set(p,{element:u,appId:t,appComponent:c,createdAt:new Date().toISOString()});const h=document.getElementById("desktop");return h&&h.appendChild(u),w(async()=>{const{default:v}=await Promise.resolve().then(()=>W);return{default:v}},void 0).then(({default:v})=>{const _=v.getCurrentIndex();v.addWindowToDesktop(_,p)}),this.focusWindow(p),u.addEventListener("window-close",()=>{this.closeWindow(p)}),u.addEventListener("window-focus",()=>{this.focusWindow(p)}),s.emit("window:created",{windowId:p,appId:t}),u}closeWindow(e){const t=this._windows.get(e);if(!t)return;const{element:i,appId:n}=t;w(async()=>{const{default:o}=await Promise.resolve().then(()=>W);return{default:o}},void 0).then(({default:o})=>{const a=o.getDesktopForWindow(e);a>=0&&o.removeWindowFromDesktop(a,e)}),i.remove(),this._windows.delete(e),this._focusedWindow===e&&(this._focusedWindow=null),s.emit("window:closed",{windowId:e,appId:n})}focusWindow(e){const t=this._windows.get(e);t&&(this._windows.forEach((i,n)=>{n!==e&&i.element.removeAttribute("focused")}),t.element.setAttribute("focused","true"),t.element.setAttribute("z-index",this._nextZIndex++),this._focusedWindow=e,s.emit("window:focused",{windowId:e,appId:t.appId,appComponent:t.appComponent}))}getWindowsForApp(e){const t=[];return this._windows.forEach(i=>{i.appId===e&&t.push(i.element)}),t}hasWindowsForApp(e){for(const t of this._windows.values())if(t.appId===e)return!0;return!1}getFocusedWindow(){if(!this._focusedWindow)return null;const e=this._windows.get(this._focusedWindow);return e?e.element:null}closeAllWindowsForApp(e){const t=[];this._windows.forEach((i,n)=>{i.appId===e&&t.push(n)}),t.forEach(i=>this.closeWindow(i))}hideApp(e){this._windows.forEach(t=>{t.appId===e&&(t.element.style.display="none",t.hidden=!0)}),s.emit("app:hidden",{appId:e})}showApp(e){this._windows.forEach(t=>{t.appId===e&&t.hidden&&(t.element.style.display="block",t.hidden=!1)}),s.emit("app:shown",{appId:e})}hideOthers(e){this._windows.forEach(t=>{t.appId!==e&&(t.element.style.display="none",t.hidden=!0)}),s.emit("app:hide-others",{exceptAppId:e})}showAll(){this._windows.forEach(e=>{e.hidden&&(e.element.style.display="block",e.hidden=!1)}),s.emit("app:show-all")}minimizeFocusedWindow(){const e=this.getFocusedWindow();e&&e.minimize()}maximizeFocusedWindow(){const e=this.getFocusedWindow();e&&e.maximize()}closeFocusedWindow(){this._focusedWindow&&this.closeWindow(this._focusedWindow)}getFocusedAppId(){if(!this._focusedWindow)return null;const e=this._windows.get(this._focusedWindow);return e?e.appId:null}}const k=new Ie,nt=Object.freeze(Object.defineProperty({__proto__:null,default:k},Symbol.toStringTag,{value:"Module"}));class Me{constructor(){this._desktops=[],this._currentDesktopIndex=0,this._initialized=!1}initialize(e=3){if(this._initialized){console.warn("[DesktopManager] Already initialized");return}for(let t=0;t<e;t++)this._desktops.push({id:`desktop-${t+1}`,index:t,name:`Desktop ${t+1}`,windows:[],widgets:[],wallpaper:null});this._initialized=!0,this._loadState(),console.log(`[DesktopManager] Initialized with ${e} desktops`)}getCurrentDesktop(){return this._desktops[this._currentDesktopIndex]}getCurrentIndex(){return this._currentDesktopIndex}getAllDesktops(){return[...this._desktops]}getDesktop(e){return e>=0&&e<this._desktops.length?this._desktops[e]:null}switchToDesktop(e){if(e<0||e>=this._desktops.length)return console.warn(`[DesktopManager] Invalid desktop index: ${e}`),!1;if(e===this._currentDesktopIndex)return!0;const t=this._currentDesktopIndex,i=this._desktops[t],n=this._desktops[e];return s.emit("desktop:before-switch",{from:t,to:e,fromDesktop:i,toDesktop:n}),this._currentDesktopIndex=e,s.emit("desktop:switched",{from:t,to:e,fromDesktop:i,toDesktop:n}),this._saveState(),console.log(`[DesktopManager] Switched from desktop ${t+1} to ${e+1}`),!0}nextDesktop(){const e=(this._currentDesktopIndex+1)%this._desktops.length;return this.switchToDesktop(e)}previousDesktop(){const e=(this._currentDesktopIndex-1+this._desktops.length)%this._desktops.length;return this.switchToDesktop(e)}createDesktop(e){const t=this._desktops.length,i={id:`desktop-${t+1}`,index:t,name:e||`Desktop ${t+1}`,windows:[],widgets:[],wallpaper:null};return this._desktops.push(i),s.emit("desktop:created",{desktop:i,index:t}),this._saveState(),console.log(`[DesktopManager] Created desktop ${t+1}: ${i.name}`),i}deleteDesktop(e){if(this._desktops.length<=1)return console.warn("[DesktopManager] Cannot delete the last desktop"),!1;if(e<0||e>=this._desktops.length)return console.warn(`[DesktopManager] Invalid desktop index: ${e}`),!1;const t=this._desktops[e];if(e===this._currentDesktopIndex){const i=e>0?e-1:0;this.switchToDesktop(i)}return this._desktops.splice(e,1),this._desktops.forEach((i,n)=>{i.index=n}),this._currentDesktopIndex>=this._desktops.length&&(this._currentDesktopIndex=this._desktops.length-1),s.emit("desktop:deleted",{desktop:t,index:e}),this._saveState(),console.log(`[DesktopManager] Deleted desktop ${e+1}`),!0}renameDesktop(e,t){const i=this.getDesktop(e);return i?(i.name=t,s.emit("desktop:renamed",{desktop:i,index:e,name:t}),this._saveState(),!0):!1}addWindowToDesktop(e,t){const i=this.getDesktop(e);i&&!i.windows.includes(t)&&(i.windows.push(t),this._saveState())}removeWindowFromDesktop(e,t){const i=this.getDesktop(e);i&&(i.windows=i.windows.filter(n=>n!==t),this._saveState())}moveWindowToDesktop(e,t,i){this.removeWindowFromDesktop(t,e),this.addWindowToDesktop(i,e),s.emit("desktop:window-moved",{windowId:e,from:t,to:i})}getDesktopForWindow(e){for(let t=0;t<this._desktops.length;t++)if(this._desktops[t].windows.includes(e))return t;return-1}isWindowOnCurrentDesktop(e){return this.getCurrentDesktop().windows.includes(e)}_saveState(){try{const e={currentDesktopIndex:this._currentDesktopIndex,desktops:this._desktops.map(o=>({id:o.id,name:o.name,wallpaper:o.wallpaper}))};g.getNode("/Users/Guest/Library/Preferences")||g.createFolder("/Users/Guest/Library","Preferences");const i="/Users/Guest/Library/Preferences/com.jaredlikes.desktop-manager.json";g.getNode(i)?g.updateFile(i,JSON.stringify(e,null,2),"application/json"):g.createFile("/Users/Guest/Library/Preferences","com.jaredlikes.desktop-manager.json",JSON.stringify(e,null,2),"application/json")}catch(e){console.error("[DesktopManager] Failed to save state:",e)}}_loadState(){try{const e=g.getNode("/Users/Guest/Library/Preferences/com.jaredlikes.desktop-manager.json");if(!e||!e.content)return;const t=JSON.parse(e.content);t.desktops&&t.desktops.forEach((i,n)=>{this._desktops[n]&&(this._desktops[n].name=i.name,this._desktops[n].wallpaper=i.wallpaper)}),typeof t.currentDesktopIndex=="number"&&(this._currentDesktopIndex=Math.min(t.currentDesktopIndex,this._desktops.length-1)),console.log("[DesktopManager] State loaded")}catch(e){console.error("[DesktopManager] Failed to load state:",e)}}}const N=new Me,W=Object.freeze(Object.defineProperty({__proto__:null,default:N},Symbol.toStringTag,{value:"Module"})),ze={small:{width:80,height:40},medium:{width:170,height:40},large:{width:170,height:170},"extra-large":{width:350,height:90},super:{width:350,height:190},"extra-super":{width:350,height:350}},De=50;class Le{constructor(){this._widgetLibrary=new Map,this._instances=new Map,this._groups=new Map,this._layout=[],this._editMode=!1,this._setupDesktopIntegration()}_setupDesktopIntegration(){s.on("desktop:switched",({from:e,to:t})=>{this._handleDesktopSwitch(e,t)})}_handleDesktopSwitch(e,t){this._instances.forEach((i,n)=>{const o=document.getElementById(`widget-instance-${n}`);o&&((i.desktopIndex!==void 0?i.desktopIndex:0)===t?o.style.display="":o.style.display="none")})}static getStandardSizes(){return ze}static getGridSize(){return De}registerWidget(e){const{type:t,name:i,componentTag:n,appId:o,persistent:a=!1}=e;if(!t||!i||!n||!o)return console.error("[WidgetManager] Widget registration missing required fields:",e),!1;if(this._widgetLibrary.has(t))return console.warn(`[WidgetManager] Widget type ${t} already registered`),!1;const d={type:t,name:i,description:e.description||"",icon:e.icon||"📦",componentTag:n,appId:o,defaultSize:e.defaultSize||{width:200,height:200},defaultProps:e.defaultProps||{},configurableProps:e.configurableProps||[],persistent:a,registeredAt:Date.now()};return this._widgetLibrary.set(t,d),s.emit("widget:registered",{widget:d}),console.log(`[WidgetManager] Registered widget: ${t} from app: ${o} (persistent: ${a})`),!0}unregisterWidget(e){if(!this._widgetLibrary.get(e))return console.warn(`[WidgetManager] Widget type ${e} not found`),!1;const i=[];return this._instances.forEach((n,o)=>{n.type===e&&i.push(o)}),i.forEach(n=>this.removeInstance(n)),this._widgetLibrary.delete(e),s.emit("widget:unregistered",{widgetType:e}),console.log(`[WidgetManager] Unregistered widget: ${e}`),!0}unregisterAppWidgets(e,t=!1){const i=[];this._widgetLibrary.forEach((n,o)=>{n.appId===e&&(!n.persistent||t)&&i.push(o)}),i.forEach(n=>this.unregisterWidget(n))}getWidgetLibrary(){return Array.from(this._widgetLibrary.values())}getWidgetDefinition(e){return this._widgetLibrary.get(e)||null}createInstance(e){const{type:t,x:i,y:n,width:o,height:a,props:d={},desktopIndex:l}=e,c=this._widgetLibrary.get(t);if(!c)return console.error(`[WidgetManager] Widget type ${t} not found in library`),null;let p=l!==void 0?l:0;if(l===void 0&&window.desktopManager)try{p=window.desktopManager.getCurrentIndex()}catch{p=0}const u=this._generateId(),h={id:u,type:t,x:i,y:n,width:o||c.defaultSize.width,height:a||c.defaultSize.height,props:{...c.defaultProps,...d},groupId:null,desktopIndex:p,createdAt:Date.now()};return this._instances.set(u,h),this._layout.push({type:"instance",id:u}),s.emit("widget:instance-created",{instance:h}),this._saveLayout(),h}removeInstance(e){const t=this._instances.get(e);return t?(t.groupId&&this.removeFromGroup(e),this._layout=this._layout.filter(i=>!(i.type==="instance"&&i.id===e)),this._instances.delete(e),s.emit("widget:instance-removed",{instanceId:e}),this._saveLayout(),!0):(console.warn(`[WidgetManager] Instance ${e} not found`),!1)}updateInstancePosition(e,t,i){const n=this._instances.get(e);if(!n){console.warn(`[WidgetManager] Instance ${e} not found`);return}n.x=t,n.y=i,s.emit("widget:instance-updated",{instanceId:e,instance:n}),this._saveLayout()}updateInstanceSize(e,t,i){const n=this._instances.get(e);if(!n){console.warn(`[WidgetManager] Instance ${e} not found`);return}n.width=t,n.height=i,s.emit("widget:instance-updated",{instanceId:e,instance:n}),this._saveLayout()}updateInstanceProps(e,t){const i=this._instances.get(e);if(!i){console.warn(`[WidgetManager] Instance ${e} not found`);return}i.props={...i.props,...t},s.emit("widget:instance-updated",{instanceId:e,instance:i}),this._saveLayout()}getInstances(){return Array.from(this._instances.values())}getInstance(e){return this._instances.get(e)||null}createGroup(e){const{x:t,y:i,columns:n=2,rows:o=2,gap:a=8}=e,d=this._generateId(),l={id:d,x:t,y:i,columns:n,rows:o,gap:a,cells:[],createdAt:Date.now()};return this._groups.set(d,l),this._layout.push({type:"group",id:d}),s.emit("widget:group-created",{group:l}),this._saveLayout(),l}removeGroup(e,t=!1){const i=this._groups.get(e);return i?(t?i.cells.forEach(n=>{const o=this._instances.get(n.instanceId);o&&(o.groupId=null)}):i.cells.forEach(n=>{this.removeInstance(n.instanceId)}),this._layout=this._layout.filter(n=>!(n.type==="group"&&n.id===e)),this._groups.delete(e),s.emit("widget:group-removed",{groupId:e}),this._saveLayout(),!0):(console.warn(`[WidgetManager] Group ${e} not found`),!1)}addToGroup(e,t,i,n){const o=this._instances.get(e),a=this._groups.get(t);if(!o||!a){console.warn("[WidgetManager] Instance or group not found");return}o.groupId&&this.removeFromGroup(e);const d=a.cells.find(l=>l.row===i&&l.col===n);d&&this.removeFromGroup(d.instanceId),o.groupId=t,a.cells.push({row:i,col:n,instanceId:e}),s.emit("widget:instance-grouped",{instanceId:e,groupId:t,row:i,col:n}),this._saveLayout()}removeFromGroup(e){const t=this._instances.get(e);if(!t||!t.groupId)return;const i=this._groups.get(t.groupId);i&&(i.cells=i.cells.filter(n=>n.instanceId!==e)),t.groupId=null,s.emit("widget:instance-ungrouped",{instanceId:e}),this._saveLayout()}getGroups(){return Array.from(this._groups.values())}getGroup(e){return this._groups.get(e)||null}getLayout(){return this._layout.map(e=>e.type==="instance"?{type:"instance",instance:this._instances.get(e.id)}:{type:"group",group:this._groups.get(e.id)})}setEditMode(e){this._editMode=e,s.emit("widget:edit-mode-changed",{editMode:e})}isEditMode(){return this._editMode}clearAll(){this._instances.clear(),this._groups.clear(),this._layout=[],s.emit("widget:layout-cleared"),this._saveLayout()}async _saveLayout(){try{const e={instances:Array.from(this._instances.entries()),groups:Array.from(this._groups.entries()),layout:this._layout};g.getNode("/Users/Guest/Library/LikesOS")||await g.createFolder("/Users/Guest/Library","LikesOS"),g.getNode("/Users/Guest/Library/LikesOS/widget-layout.json")?await g.updateFile("/Users/Guest/Library/LikesOS/widget-layout.json",JSON.stringify(e,null,2),"application/json"):await g.createFile("/Users/Guest/Library/LikesOS","widget-layout.json",JSON.stringify(e,null,2),"application/json")}catch(e){console.error("[WidgetManager] Failed to save layout:",e)}}async loadLayout(){try{const e=g.getNode("/Users/Guest/Library/LikesOS/widget-layout.json");if(!e||!e.content)return;const t=JSON.parse(e.content),i=new Map;for(const[n,o]of t.instances)this._widgetLibrary.has(o.type)?i.set(n,o):console.warn(`[WidgetManager] Skipping widget with invalid type: ${o.type}`);if(this._instances=i,this._groups=new Map(t.groups),this._layout=(t.layout||[]).filter(n=>n.type==="instance"?i.has(n.id):n.type==="group"?this._groups.has(n.id):!0),window.desktopManager){const n=window.desktopManager.getCurrentIndex();setTimeout(()=>{this._instances.forEach((o,a)=>{const d=document.getElementById(`widget-instance-${a}`),l=o.desktopIndex!==void 0?o.desktopIndex:0;d&&l!==n&&(d.style.display="none")})},100)}s.emit("widget:layout-loaded"),console.log("[WidgetManager] Layout loaded from storage")}catch(e){console.error("[WidgetManager] Failed to load layout:",e)}}_generateId(){return`widget_${Date.now()}_${Math.random().toString(36).substr(2,9)}`}}const m=new Le;s.on("app:terminated",({appId:r})=>{m.unregisterAppWidgets(r)});const Te=[{id:"aiua",name:"Aiua",title:"Aiua",initialTab:"mesh",icon:"⚡",dockLabel:"Aiua"},{id:"aiua-mesh",name:"Aiua Mesh",title:"Aiua Mesh",initialTab:"mesh",icon:"🕸️",dockLabel:"Mesh"},{id:"aiua-agents",name:"Aiua Agents",title:"Aiua Agents",initialTab:"agents",icon:"🤖",dockLabel:"Agents"},{id:"aiua-components",name:"Aiua Components",title:"Aiua Components",initialTab:"components",icon:"⚙️",dockLabel:"Parts"},{id:"aiua-config",name:"Aiua Config",title:"Aiua Config",initialTab:"config",icon:"🔑",dockLabel:"Config"},{id:"aiua-catalog",name:"Aiua Catalog",title:"Aiua Catalog",initialTab:"catalog",icon:"📖",dockLabel:"Catalog"}];async function $e(){for(const r of Te)C.registerApplication({id:r.id,name:r.name,icon:r.icon,version:"0.1.0",canQuit:!0,terminateWhenNoWindows:!1,capabilities:["network","storage"]}),b.register({id:r.id,name:r.name,icon:r.icon,iconUrl:"",dockLabel:r.dockLabel,permissions:{filesystem:!1,network:!0,storage:!0},onLaunch:async(e={})=>{await w(()=>import("./aiua-app-BGh3FSa8.js"),__vite__mapDeps([0,1])),await w(()=>import("./aiua-tab-bar-BsSw2IRX.js"),[]),await w(()=>import("./aiua-guests-panel-CcI4R-Rv.js"),[]),await w(()=>import("./aiua-agents-panel-DgYp1_vb.js"),[]),await w(()=>import("./aiua-agent-window-BMvwauCi.js"),[]),await w(()=>import("./aiua-role-window-8UHUTw6M.js"),[]),await w(()=>import("./aiua-sessions-panel-cVdBaj8m.js"),[]),await w(()=>import("./aiua-components-panel-RRiVEYez.js"),[]),await w(()=>import("./aiua-component-window-C2t_M7Pr.js"),[]),await w(()=>import("./aiua-graphs-panel-CDd4sMSH.js"),[]),await w(()=>import("./aiua-config-panel-D-hJx9cz.js"),[]),await w(()=>import("./aiua-agent-chat-panel-ax_i8Pdi.js"),[]),await w(()=>import("./aiua-catalog-panel-DdonuZtD.js"),[]),await w(()=>import("./aiua-cron-panel-BiErXSfH.js"),[]),await w(()=>import("./aiua-user-profile-panel-CmN6NGS2.js"),[]),await I.initialize();const t=e.initialTab||r.initialTab||"mesh",i=e.targetNodeId||"",n=[`initial-tab="${t}"`];i&&n.push(`target-node-id="${i}"`);const o=n.join(" ");return k.createWindow({appId:r.id,title:e.title||r.title,x:e.x||180,y:e.y||120,width:e.width||980,height:e.height||660,minWidth:720,minHeight:500,content:`<aiua-app ${o}></aiua-app>`})},onQuit:async()=>{s.emit("aiua:app-quit",{appId:r.id})}});Pe(),Re(),await I.initialize(),I.isConnected()&&!b.isRunning("aiua")&&b.launch("aiua"),console.log("[Aiua App] Initialized — philotic-web management UI ready")}function Pe(){s.on("aiua:connected",()=>{console.log("[Aiua App] Connected to aiua at",I.getBaseUrl()),b.isRunning("aiua")||b.launch("aiua")}),s.on("aiua:disconnected",()=>{console.log("[Aiua App] Disconnected from aiua")}),s.on("aiua:ws-connected",()=>{console.log("[Aiua App] WebSocket live")}),s.on("keyboard:cmd-shift-a",()=>{b.isRunning("aiua")?b.focus?.("aiua"):b.launch("aiua")})}function Re(){s.on("window:created",r=>{r.appId?.startsWith("aiua")&&console.log("[Aiua App] Window created:",r.windowId,r.appId)}),s.on("window:closed",r=>{r.appId?.startsWith("aiua")&&console.log("[Aiua App] Window closed:",r.windowId,r.appId)})}function V(r,e){const t=`${r}-${e}`;let i=0;for(let n=0;n<t.length;n++){const o=t.charCodeAt(n);i=(i<<5)-i+o,i=i&i}return Math.abs(i).toString(16).substring(0,8)}const z={major:1,minor:0,patch:0,prerelease:"alpha.1",get semantic(){return this.prerelease?`${this.major}.${this.minor}.${this.patch}-${this.prerelease}`:`${this.major}.${this.minor}.${this.patch}`},codename:"Tahoe",build:"26.1",get timestamp(){return new Date().toISOString()},get hash(){return V(this.semantic,this.timestamp)},get full(){return`${this.semantic} (${this.codename} ${this.build}) [${this.hash}]`}};function x(r,e,t){return{version:r,build:e,features:t,get timestamp(){return new Date().toISOString()},get hash(){return V(`${r}-${e}`,this.timestamp)}}}const $={finder:x("1.0.0","001",["file-opening","preview-integration","context-menus"]),notes:x("1.0.0","012",["markdown","wiki-links","tags","search","command-palette"]),chat:x("1.0.0","013",["onnx-inference","streaming","handoff","history"]),preview:x("1.0.0","001",["json","xml","markdown","syntax-highlighting"]),"browser-chat":x("1.0.0","014",["cmd-k-activation","tab-handoff","streaming"]),safari:x("1.0.0","001",["basic-browser"]),mail:x("1.0.0","001",["basic-mail"]),messages:x("1.0.0","001",["basic-messages"]),music:x("1.0.0","001",["basic-music"]),weather:x("1.0.0","001",["weather-display","widget"]),focus:x("1.0.0","001",["quotes","widget"]),"activity-monitor":x("1.0.0","001",["basic-monitor"]),desktop:x("1.0.0","001",["basic-desktop"])};function F(){const r=new Date().toLocaleTimeString("en-US",{hour:"2-digit",minute:"2-digit",second:"2-digit",hour12:!1});console.log("%c╔══════════════════════════════════════════════╗","color: #007AFF; font-weight: bold;"),console.log("%c║   🍎 Likes OS - macOS Tahoe 26.1            ║","color: #007AFF; font-weight: bold;"),console.log(`%c║   📦 Version: ${z.semantic.padEnd(27)}║`,"color: #007AFF; font-weight: bold;"),console.log(`%c║   🔨 Build: ${z.hash.padEnd(29)}║`,"color: #007AFF; font-weight: bold;"),console.log(`%c║   🕐 Loaded: ${r.padEnd(28)}║`,"color: #007AFF; font-weight: bold;"),console.log("%c╚══════════════════════════════════════════════╝","color: #007AFF; font-weight: bold;"),console.log(""),console.log(`%c📝 Full Version: ${z.full}`,"color: #888;"),console.log("")}function J(){return{system:z,apps:$,timestamp:new Date().toISOString()}}const We=Object.freeze(Object.defineProperty({__proto__:null,APP_VERSIONS:$,SYSTEM_VERSION:z,getAllVersionInfo:J,printVersionBanner:F},Symbol.toStringTag,{value:"Module"}));async function Ne(){const r={id:"finder",name:"Finder",icon:"📁",version:"1.0.0",canQuit:!1,terminateWhenNoWindows:!1,capabilities:["filesystem"]};C.registerApplication(r),b.register({id:"finder",name:"Finder",icon:"📁",iconUrl:"",permissions:{filesystem:!0,network:!1},onLaunch:async(t={})=>(await w(()=>import("./finder-app-CdFc47b7.js"),__vite__mapDeps([2,1])),k.createWindow({appId:"finder",title:"Finder",x:t.x||100,y:t.y||100,width:t.width||900,height:t.height||600,minWidth:600,minHeight:400,content:"<finder-app></finder-app>"})),onQuit:async()=>(console.log("[Finder] Cannot quit Finder"),!1)});const e=$.finder;console.log(`[Finder App] Initialized v${e.version} (build ${e.build}) - Features: ${e.features.join(", ")}`)}async function Fe(){const r={id:"notes",name:"Notes",icon:"📝",version:"1.0.0",canQuit:!0,terminateWhenNoWindows:!0,capabilities:["filesystem"]};C.registerApplication(r),b.register({id:"notes",name:"Notes",icon:"📝",iconUrl:"",permissions:{filesystem:!0,network:!1},onLaunch:async(t={})=>(await w(()=>import("./notes-app-DrPuRKd0.js"),__vite__mapDeps([3,1])),k.createWindow({appId:"notes",title:"Notes",x:t.x||100,y:t.y||100,width:t.width||1200,height:t.height||800,minWidth:800,minHeight:600,content:"<notes-app></notes-app>"})),onQuit:async()=>{s.emit("notes:app-quit")}}),await He(),await Oe(),Ue(),qe();const e=$.notes;console.log(`[Notes App] Initialized v${e.version} (build ${e.build}) - Features: ${e.features.join(", ")}`)}async function He(){await g.initialize();const r=g.getNode("/Applications");if(!r){console.error("[Notes App] /Applications folder not found");return}if(r.children.find(i=>i.name==="Notes.app"))return;await g.createFile("/Applications","Notes.app","","application/x-macos-app");const t=r.children.find(i=>i.name==="Notes.app");t&&(t.type="application",t.appId="notes",t.icon="📝",await g._saveToStorage()),s.emit("filesystem:app-installed",{appId:"notes",path:"/Applications/Notes.app"})}async function Oe(){await g.initialize();const r=g.getNode("/Users/Guest/Documents");if(!r){console.error("[Notes App] /Users/Guest/Documents folder not found");return}if(r.children.find(i=>i.name==="Notes"))return;await g.createFolder("/Users/Guest/Documents","Notes"),await g.createFile("/Users/Guest/Documents/Notes","Welcome.md",`# Welcome to Notes

This is your personal knowledge management system, inspired by Obsidian.

## Features

- **Markdown editing**: Write notes using markdown syntax
- **Wiki-style links**: Link notes together using \`[[Note Name]]\` syntax
- **Tags**: Organize notes with \`#tags\`
- **Full-text search**: Find notes quickly with ⌘F
- **Split view**: Edit and preview side-by-side

## Getting Started

1. Create a new note with ⌘N
2. Link to other notes using [[My Other Note]]
3. Add tags like #important or #project
4. Use ⌘F to search across all notes

## Keyboard Shortcuts

- **⌘N** - New note
- **⌘S** - Save current note
- **⌘F** - Search notes
- **⌘W** - Close window
- **⌘⇧E** - Edit mode
- **⌘⇧P** - Preview mode
- **⌘⇧S** - Split view
- **⌘B** - Toggle sidebar
- **⌘K** - Command palette

Happy note-taking!
`,"text/markdown"),s.emit("notes:initialized",{notesPath:"/Users/Guest/Documents/Notes",welcomeNote:"/Users/Guest/Documents/Notes/Welcome.md"})}function Ue(){s.on("note:created",({note:r})=>{s.emit("filesystem:file-created",{path:r.path,file:r})}),s.on("note:saved",({note:r})=>{s.emit("filesystem:file-modified",{path:r.path,file:r})}),s.on("note:deleted",({noteId:r,path:e})=>{s.emit("filesystem:deleted",{path:e,nodeId:r})}),s.on("note:renamed",({oldPath:r,newPath:e,note:t})=>{s.emit("filesystem:renamed",{oldPath:r,newPath:e,node:t})}),s.on("note:moved",({oldPath:r,newPath:e,note:t})=>{s.emit("filesystem:moved",{sourcePath:r,destPath:e,node:t})})}function qe(){s.on("window:created",({windowId:t,appId:i})=>{i==="notes"&&s.emit("notes:window-created",{windowId:t})}),s.on("window:closed",({windowId:t,appId:i})=>{i==="notes"&&s.emit("notes:window-closed",{windowId:t})}),s.on("window:focused",({windowId:t,appId:i})=>{i==="notes"&&s.emit("notes:window-focused",{windowId:t})}),s.on("note:opened",({note:t})=>{const n=b.getRunningApps().find(o=>o.appId==="notes");if(n&&n.windows.length>0){const o=n.windows[n.windows.length-1];k.updateWindow(o,{title:`${t.name} - Notes`})}});let r=new Set;s.on("note:modified",({noteId:t})=>{r.add(t),e(t,!0)}),s.on("note:saved",({note:t})=>{r.delete(t.id),e(t.id,!1)});function e(t,i){const o=b.getRunningApps().find(a=>a.appId==="notes");o&&o.windows.length>0&&s.emit("notes:unsaved-changes",{noteId:t,hasUnsavedChanges:i})}}class Be extends HTMLElement{constructor(){super(),this.attachShadow({mode:"open"}),this._draggedWidget=null,this._dragOffset={x:0,y:0},this._isDragging=!1,this._hasRendered=!1,this._initialRenderTimeout=null}connectedCallback(){console.log("[WidgetContainer] Connected"),this._render(),this._setupEventListeners(),this._setupOverlayClickHandler(),this._setupDragAndDrop(),this._initialRenderTimeout=setTimeout(()=>{this._hasRendered||(console.log("[WidgetContainer] Initial render timeout - rendering widgets"),this._renderWidgets())},150)}disconnectedCallback(){this._cleanupEventListeners(),this._initialRenderTimeout&&clearTimeout(this._initialRenderTimeout)}_render(){const e=document.createElement("template");e.innerHTML=`
      <style>
        :host {
          position: absolute;
          top: 0;
          left: 0;
          width: 100%;
          height: 100%;
          pointer-events: none;
          z-index: 5;
          transition: z-index 0s;
        }

        :host(.edit-mode) {
          z-index: 10000;
          background: rgba(0, 0, 0, 0.3);
          backdrop-filter: blur(20px);
          -webkit-backdrop-filter: blur(20px);
          pointer-events: auto;
        }

        .widgets-layer {
          position: relative;
          width: 100%;
          height: 100%;
        }

        :host(.edit-mode) .widgets-layer {
          background-image:
            repeating-linear-gradient(0deg, rgba(255, 255, 255, 0.08) 0px, rgba(255, 255, 255, 0.08) 1px, transparent 1px, transparent 50px),
            repeating-linear-gradient(90deg, rgba(255, 255, 255, 0.08) 0px, rgba(255, 255, 255, 0.08) 1px, transparent 1px, transparent 90px);
          background-size: 90px 50px;
        }

        .widget-instance {
          position: absolute;
          pointer-events: auto;
          user-select: none;
          -webkit-user-select: none;
        }

        .widget-instance.dragging {
          opacity: 0.9;
          z-index: 1000;
        }

        .widget-instance.long-press-active {
          transform: scale(1.05);
          transition: transform 0.3s ease;
        }

        .widget-delete-btn {
          display: none;
          position: absolute;
          top: -8px;
          left: -8px;
          width: 24px;
          height: 24px;
          border-radius: 12px;
          background: var(--accent-color, #007AFF);
          border: 2px solid white;
          color: white;
          font-size: 16px;
          font-weight: bold;
          cursor: pointer;
          align-items: center;
          justify-content: center;
          z-index: 10;
          box-shadow: 0 2px 8px rgba(0, 0, 0, 0.3);
          transition: all 0.2s ease;
        }

        .widget-delete-btn:hover {
          filter: brightness(0.85);
          transform: scale(1.1);
        }

        :host(.edit-mode) .widget-delete-btn {
          display: flex;
        }

        .widget-group {
          position: absolute;
          display: grid;
          pointer-events: auto;
        }

        .widget-group-cell {
          position: relative;
        }

        .widget-group.edit-mode {
          border: 2px dashed var(--accent-color);
          background: rgba(var(--accent-color-rgb), 0.05);
          border-radius: var(--radius-lg);
        }

        .group-delete-btn {
          display: none;
          position: absolute;
          top: -8px;
          right: -8px;
          width: 24px;
          height: 24px;
          border-radius: 12px;
          background: var(--accent-color, #007AFF);
          border: 2px solid white;
          color: white;
          font-size: 16px;
          font-weight: bold;
          cursor: pointer;
          align-items: center;
          justify-content: center;
          z-index: 10;
          box-shadow: 0 2px 8px rgba(0, 0, 0, 0.3);
          transition: all 0.2s ease;
        }

        .group-delete-btn:hover {
          filter: brightness(0.85);
          transform: scale(1.1);
        }

        :host(.edit-mode) .group-delete-btn {
          display: flex;
        }
      </style>

      <div class="widgets-layer" id="widgets-layer"></div>
    `,this.shadowRoot.appendChild(e.content.cloneNode(!0))}_setupEventListeners(){this._instanceCreatedHandler=({instance:e})=>this._renderWidgets(),this._instanceRemovedHandler=()=>this._renderWidgets(),this._instanceUpdatedHandler=()=>this._renderWidgets(),this._groupCreatedHandler=()=>this._renderWidgets(),this._groupRemovedHandler=()=>this._renderWidgets(),this._editModeChangedHandler=({editMode:e})=>this._updateEditMode(e),this._layoutLoadedHandler=()=>this._renderWidgets(),this._removeRequestedHandler=({instanceId:e})=>this._handleRemoveRequest(e),this._contextMenuHandler=e=>this._showContextMenu(e),s.on("widget:instance-created",this._instanceCreatedHandler),s.on("widget:instance-removed",this._instanceRemovedHandler),s.on("widget:instance-updated",this._instanceUpdatedHandler),s.on("widget:group-created",this._groupCreatedHandler),s.on("widget:group-removed",this._groupRemovedHandler),s.on("widget:edit-mode-changed",this._editModeChangedHandler),s.on("widget:layout-loaded",this._layoutLoadedHandler),s.on("widget:remove-requested",this._removeRequestedHandler),s.on("widget:context-menu",this._contextMenuHandler)}_cleanupEventListeners(){s.off("widget:instance-created",this._instanceCreatedHandler),s.off("widget:instance-removed",this._instanceRemovedHandler),s.off("widget:instance-updated",this._instanceUpdatedHandler),s.off("widget:group-created",this._groupCreatedHandler),s.off("widget:group-removed",this._groupRemovedHandler),s.off("widget:edit-mode-changed",this._editModeChangedHandler),s.off("widget:layout-loaded",this._layoutLoadedHandler),s.off("widget:remove-requested",this._removeRequestedHandler),s.off("widget:context-menu",this._contextMenuHandler)}_renderWidgets(){if(this._isDragging){console.log("[WidgetContainer] Skipping re-render during drag");return}this._hasRendered=!0,this._initialRenderTimeout&&(clearTimeout(this._initialRenderTimeout),this._initialRenderTimeout=null);const e=this.shadowRoot.getElementById("widgets-layer");if(!e){console.error("[WidgetContainer] Layer not found!");return}e.querySelectorAll(".widget-instance").forEach(n=>{n._cleanupDrag&&n._cleanupDrag(),n._cleanupEditModeActivation&&n._cleanupEditModeActivation()}),e.innerHTML="";const i=m.getLayout();console.log("[WidgetContainer] Rendering widgets, layout:",i),i.forEach(n=>{n.type==="instance"&&n.instance?(console.log("[WidgetContainer] Rendering instance:",n.instance),this._renderInstance(e,n.instance)):n.type==="group"&&n.group&&this._renderGroup(e,n.group)})}_renderInstance(e,t){if(t.groupId)return;console.log("[WidgetContainer] Attempting to render instance:",t);const i=m.getWidgetDefinition(t.type);if(console.log("[WidgetContainer] Widget definition for type",t.type,":",i),!i){console.error(`[WidgetContainer] Widget definition not found: ${t.type}`),console.error("[WidgetContainer] Available widget types:",Array.from(m.getWidgetLibrary()).map(d=>d.type));return}console.log("[WidgetContainer] Creating wrapper for instance:",t.id,"position:",t.x,t.y);const n=document.createElement("div");if(n.className="widget-instance",n.id=`widget-instance-${t.id}`,n.style.left=`${t.x}px`,n.style.top=`${t.y}px`,n.style.width=`${t.width}px`,n.style.height=`${t.height}px`,n.dataset.instanceId=t.id,window.desktopManager){const d=window.desktopManager.getCurrentIndex();(t.desktopIndex!==void 0?t.desktopIndex:0)!==d&&(n.style.display="none")}const o=document.createElement("button");o.className="widget-delete-btn",o.innerHTML="−",o.title="Remove widget",o.addEventListener("click",d=>{d.stopPropagation(),console.log("[WidgetContainer] Delete button clicked for:",t.id),m.removeInstance(t.id)}),n.appendChild(o);const a=document.createElement(i.componentTag);a.setInstanceId(t.id),a.setProps(t.props),a.setEditMode(m.isEditMode()),n.appendChild(a),e.appendChild(n),console.log("[WidgetContainer] Widget appended to parent, edit mode:",m.isEditMode()),m.isEditMode()?(console.log("[WidgetContainer] Enabling dragging for widget:",t.id),this._enableDragging(n,t)):this._enableEditModeActivation(n,t)}_renderGroup(e,t){const i=document.createElement("div");i.className="widget-group",i.style.left=`${t.x}px`,i.style.top=`${t.y}px`,i.style.gridTemplateColumns=`repeat(${t.columns}, 1fr)`,i.style.gridTemplateRows=`repeat(${t.rows}, 1fr)`,i.style.gap=`${t.gap}px`,i.dataset.groupId=t.id,m.isEditMode()&&i.classList.add("edit-mode");const n=200,o=200,a=n*t.columns+t.gap*(t.columns-1),d=o*t.rows+t.gap*(t.rows-1);i.style.width=`${a}px`,i.style.height=`${d}px`;for(let l=0;l<t.rows;l++)for(let c=0;c<t.columns;c++){const p=document.createElement("div");p.className="widget-group-cell",p.dataset.row=l,p.dataset.col=c;const u=t.cells.find(h=>h.row===l&&h.col===c);if(u){const h=m.getInstance(u.instanceId);if(h){const v=m.getWidgetDefinition(h.type);if(v){const _=document.createElement(v.componentTag);_.setInstanceId(h.id),_.setProps(h.props),_.setEditMode(m.isEditMode()),p.appendChild(_)}}}i.appendChild(p)}e.appendChild(i)}_enableEditModeActivation(e,t){let i=null;const n=500;let o=!1;const a=()=>{if(o){console.log("[WidgetContainer] Edit mode activation already in progress, ignoring");return}o=!0,console.log("[WidgetContainer] Entering edit mode (dialog tabbed/collapsed)"),navigator.vibrate&&navigator.vibrate(50),m.setEditMode(!0);const c=document.getElementById("widget-editor");c&&(c.hasAttribute("open")?c.collapse():c.openCollapsed()),setTimeout(()=>{o=!1},100)},d=c=>{console.log("[WidgetContainer] Double-click detected on widget",t.id),c.preventDefault(),c.stopPropagation(),a()},l=c=>{if(c.button!==0)return;const p=c.clientX,u=c.clientY,h=Date.now();i=setTimeout(()=>{console.log("[WidgetContainer] Long press detected on widget",t.id),a()},n);const v=S=>{Math.sqrt(Math.pow(S.clientX-p,2)+Math.pow(S.clientY-u,2))>5&&clearTimeout(i)},_=S=>{Date.now()-h<n&&clearTimeout(i),document.removeEventListener("mousemove",v),document.removeEventListener("mouseup",_)};document.addEventListener("mousemove",v),document.addEventListener("mouseup",_)};e.addEventListener("dblclick",d),e.addEventListener("mousedown",l),e._cleanupEditModeActivation=()=>{e.removeEventListener("dblclick",d),e.removeEventListener("mousedown",l),i&&clearTimeout(i)}}_enableDragging(e,t){let i=null,n=!0,o=!1;const a=500,d=l=>{console.log("[WidgetContainer] Mouse down on widget",t.id);const c=l.clientX,p=l.clientY;o=!1,i=setTimeout(()=>{console.log("[WidgetContainer] Long press detected - opening widget editor"),n=!1,navigator.vibrate&&navigator.vibrate(50);const y=document.getElementById("widget-editor");y&&!y.hasAttribute("open")&&y.open()},a);const u=e.getBoundingClientRect(),h=l.clientX-u.left,v=l.clientY-u.top,_=y=>{if(Math.sqrt(Math.pow(y.clientX-c,2)+Math.pow(y.clientY-p,2))>5&&(clearTimeout(i),n=!0),!n)return;o=!0,y.preventDefault(),this._isDragging=!0,e.classList.add("dragging"),console.log("[WidgetContainer] Dragging widget",t.id);const H=y.clientX-h,D=y.clientY-v;e.style.left=`${H}px`,e.style.top=`${D}px`},S=()=>{if(console.log("[WidgetContainer] Mouse up, dragging enabled:",n,"moved:",o),clearTimeout(i),this._isDragging=!1,e.classList.remove("dragging"),e.classList.remove("long-press-active"),n&&o){let y=parseInt(e.style.left),E=parseInt(e.style.top);const D=this.shadowRoot.getElementById("widgets-layer").getBoundingClientRect(),O=D.width,U=D.height,P=t.width,R=t.height,A=15;y=Math.max(A,Math.min(y,O-P-A)),E=Math.max(A,Math.min(E,U-R-A));const q=90,B=50;let L=Math.round(y/q)*q,T=Math.round(E/B)*B;if(L=Math.max(A,Math.min(L,O-P-A)),T=Math.max(A,Math.min(T,U-R-A)),this._checkCollision(t.id,L,T,P,R)){console.log("[WidgetContainer] Collision detected, reverting to original position"),e.style.left=`${t.x}px`,e.style.top=`${t.y}px`;return}y=L,E=T,e.style.left=`${y}px`,e.style.top=`${E}px`,console.log("[WidgetContainer] Saving snapped position:",y,E),m.updateInstancePosition(t.id,y,E)}n=!1,o=!1,document.removeEventListener("mousemove",_),document.removeEventListener("mouseup",S)};document.addEventListener("mousemove",_),document.addEventListener("mouseup",S),l.preventDefault(),l.stopPropagation()};e.addEventListener("mousedown",d),e._cleanupDrag=()=>{e.removeEventListener("mousedown",d),i&&clearTimeout(i)}}_updateEditMode(e){console.log("[WidgetContainer] Update edit mode:",e),e?this.classList.add("edit-mode"):this.classList.remove("edit-mode"),this.shadowRoot.querySelectorAll(".widget-instance").forEach(n=>{const o=n.querySelector("[is-instance-id]");o&&o.setEditMode&&o.setEditMode(e)}),this.shadowRoot.querySelectorAll(".widget-group").forEach(n=>{e?n.classList.add("edit-mode"):n.classList.remove("edit-mode"),n.querySelectorAll("[is-instance-id]").forEach(a=>{a.setEditMode&&a.setEditMode(e)})}),this._renderWidgets()}_handleRemoveRequest(e){m.removeInstance(e)}_showContextMenu({instanceId:e,widgetType:t,x:i,y:n}){console.log("[WidgetContainer] Showing context menu for widget:",e),this._hideContextMenu();const o=document.createElement("div");o.className="widget-context-menu",o.style.left=`${i}px`,o.style.top=`${n}px`,o.innerHTML=`
      <style>
        .widget-context-menu {
          position: fixed;
          background: rgba(255, 255, 255, 0.95);
          backdrop-filter: blur(40px) saturate(180%);
          -webkit-backdrop-filter: blur(40px) saturate(180%);
          border-radius: 8px;
          border: 1px solid rgba(0, 0, 0, 0.1);
          box-shadow: 0 8px 32px rgba(0, 0, 0, 0.2);
          padding: 4px;
          min-width: 180px;
          z-index: 10000;
          font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Text', sans-serif;
          font-size: 13px;
        }

        .context-menu-item {
          padding: 8px 12px;
          cursor: pointer;
          border-radius: 4px;
          color: #000;
          display: flex;
          align-items: center;
          gap: 8px;
          transition: background 0.1s ease;
        }

        .context-menu-item:hover {
          background: rgba(0, 122, 255, 0.8);
          color: white;
        }

        .context-menu-item.danger {
          color: #FF3B30;
        }

        .context-menu-item.danger:hover {
          background: #FF3B30;
          color: white;
        }

        .context-menu-separator {
          height: 1px;
          background: rgba(0, 0, 0, 0.1);
          margin: 4px 0;
        }
      </style>
      <div class="context-menu-item" data-action="edit">
        <span>✏️</span>
        <span>Edit Widget</span>
      </div>
      <div class="context-menu-separator"></div>
      <div class="context-menu-item danger" data-action="remove">
        <span>🗑️</span>
        <span>Remove Widget</span>
      </div>
    `,o.querySelectorAll(".context-menu-item").forEach(a=>{a.addEventListener("click",d=>{d.stopPropagation();const l=a.dataset.action;if(l==="edit"){console.log("[WidgetContainer] Opening widget editor"),m.setEditMode(!0);const c=document.getElementById("widget-editor");c&&c.open()}else l==="remove"&&(console.log("[WidgetContainer] Removing widget:",e),m.removeInstance(e));this._hideContextMenu()})}),this._contextMenuOutsideClickHandler=a=>{o.contains(a.target)||this._hideContextMenu()},document.body.appendChild(o),this._activeContextMenu=o,setTimeout(()=>{document.addEventListener("click",this._contextMenuOutsideClickHandler)},10)}_hideContextMenu(){this._activeContextMenu&&(this._activeContextMenu.remove(),this._activeContextMenu=null),this._contextMenuOutsideClickHandler&&(document.removeEventListener("click",this._contextMenuOutsideClickHandler),this._contextMenuOutsideClickHandler=null)}_setupOverlayClickHandler(){this.addEventListener("click",e=>{if(e.target===this&&this.classList.contains("edit-mode")){console.log("[WidgetContainer] Overlay clicked, closing editor"),m.setEditMode(!1);const t=document.getElementById("widget-editor");t&&t.close()}}),this.addEventListener("dblclick",e=>{if(e.target===this&&this.classList.contains("edit-mode")){console.log("[WidgetContainer] Overlay double-clicked, opening/expanding widget editor"),e.preventDefault(),e.stopPropagation();const t=document.getElementById("widget-editor");t&&(t.hasAttribute("open")?t.expand():t.open())}})}_checkCollision(e,t,i,n,o){const a=m.getInstances().filter(d=>d.id!==e);for(const d of a){const l=t<d.x+d.width&&t+n>d.x,c=i<d.y+d.height&&i+o>d.y;if(l&&c)return console.log("[WidgetContainer] Collision detected with widget:",d.id),!0}return!1}_setupDragAndDrop(){const e=this.shadowRoot.getElementById("widgets-layer");e&&(e.addEventListener("dragover",t=>{t.preventDefault(),t.dataTransfer.dropEffect="copy"}),e.addEventListener("drop",t=>{t.preventDefault();const i=t.dataTransfer.getData("text/plain");if(!i)return;console.log("[WidgetContainer] Dropped widget type:",i);const n=e.getBoundingClientRect();let o=t.clientX-n.left,a=t.clientY-n.top;const d=90,l=50;o=Math.round(o/d)*d,a=Math.round(a/l)*l;const c=m.getWidgetDefinition(i);if(!c){console.error("[WidgetContainer] Widget definition not found:",i);return}const p=c.defaultSize.width,u=c.defaultSize.height;if(this._checkCollision(null,o,a,p,u)){console.log("[WidgetContainer] Cannot drop widget here - collision detected");return}m.createInstance({type:i,x:o,y:a}),console.log("[WidgetContainer] Widget created at:",o,a)}))}}customElements.define("widget-container",Be);class Z extends HTMLElement{constructor(){super(),this.attachShadow({mode:"open"}),this._instanceId=null,this._widgetType=null,this._props={},this._editMode=!1}connectedCallback(){this._render(),this._setupContextMenu(),this.setupEventListeners()}disconnectedCallback(){this._cleanupContextMenu(),this.cleanup()}getWidgetInfo(){throw new Error("getWidgetInfo() must be implemented by subclass")}renderContent(){throw new Error("renderContent() must be implemented by subclass")}setupEventListeners(){}cleanup(){}onPropsChanged(e){}setInstanceId(e){this._instanceId=e}getInstanceId(){return this._instanceId}setProps(e){this._props={...this._props,...e},this.onPropsChanged(this._props)}getProps(){return{...this._props}}setEditMode(e){this._editMode=e;const t=this.shadowRoot.querySelector(".widget-container");t&&(e?t.classList.add("edit-mode"):t.classList.remove("edit-mode"))}update(){const e=this.shadowRoot.querySelector(".widget-content");e&&(e.innerHTML=this.renderContent())}_render(){const e=this.getWidgetInfo(),t=document.createElement("template");t.innerHTML=`
      <style>
        /* ===== Widget Design System ===== */
        /* Standard fonts, sizes, colors, and spacing for all widgets */

        :host {
          display: block;
          width: 100%;
          height: 100%;
          contain: layout style paint;
          overflow: hidden;
          border-radius: 20px;

          /* CSS Variables for widget design system */
          --widget-font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Display', 'Segoe UI', Roboto, sans-serif;
          --widget-font-family-mono: 'SF Mono', Monaco, 'Cascadia Code', 'Roboto Mono', Consolas, 'Courier New', monospace;

          /* Font sizes */
          --widget-font-size-xs: 10px;
          --widget-font-size-sm: 12px;
          --widget-font-size-base: 14px;
          --widget-font-size-lg: 16px;
          --widget-font-size-xl: 20px;
          --widget-font-size-2xl: 24px;
          --widget-font-size-3xl: 32px;
          --widget-font-size-4xl: 48px;

          /* Font weights */
          --widget-font-weight-light: 300;
          --widget-font-weight-regular: 400;
          --widget-font-weight-medium: 500;
          --widget-font-weight-semibold: 600;
          --widget-font-weight-bold: 700;

          /* Line heights */
          --widget-line-height-tight: 1.2;
          --widget-line-height-normal: 1.5;
          --widget-line-height-relaxed: 1.75;

          /* Spacing scale */
          --widget-space-1: 4px;
          --widget-space-2: 8px;
          --widget-space-3: 12px;
          --widget-space-4: 16px;
          --widget-space-5: 20px;
          --widget-space-6: 24px;
          --widget-space-8: 32px;

          /* Colors */
          --widget-text-primary: rgba(255, 255, 255, 0.95);
          --widget-text-secondary: rgba(255, 255, 255, 0.75);
          --widget-text-tertiary: rgba(255, 255, 255, 0.55);
          --widget-text-quaternary: rgba(255, 255, 255, 0.35);

          /* Background */
          --widget-bg: rgba(255, 255, 255, 0.25);
          --widget-bg-hover: rgba(255, 255, 255, 0.35);
          --widget-border: rgba(255, 255, 255, 0.3);

          /* Shadows */
          --widget-shadow-sm: 0 2px 8px rgba(0, 0, 0, 0.08);
          --widget-shadow-md: 0 8px 32px rgba(0, 0, 0, 0.1);
          --widget-shadow-lg: 0 12px 48px rgba(0, 0, 0, 0.15);
        }

        .widget-container {
          width: 100%;
          height: 100%;
          background: var(--widget-bg);
          backdrop-filter: blur(40px) saturate(180%);
          -webkit-backdrop-filter: blur(40px) saturate(180%);
          border-radius: 20px;
          border: 1px solid var(--widget-border);
          box-shadow: var(--widget-shadow-md);
          overflow: hidden;
          display: flex;
          flex-direction: column;
          transition: all 0.2s ease;
          box-sizing: border-box;

          /* Standard typography */
          font-family: var(--widget-font-family);
          font-size: var(--widget-font-size-base);
          font-weight: var(--widget-font-weight-regular);
          line-height: var(--widget-line-height-normal);
          color: var(--widget-text-primary);
          -webkit-font-smoothing: antialiased;
          -moz-osx-font-smoothing: grayscale;
        }

        .widget-container:hover {
          box-shadow: var(--widget-shadow-lg);
        }

        .widget-container.edit-mode {
          border: 2px solid rgba(255, 255, 255, 0.5);
        }

        .widget-header {
          display: none;
        }

        .widget-title {
          display: flex;
          align-items: center;
          gap: var(--widget-space-2);
          font-family: var(--widget-font-family);
          font-size: var(--widget-font-size-xs);
          font-weight: var(--widget-font-weight-semibold);
          color: var(--widget-text-primary);
          text-transform: uppercase;
          letter-spacing: 0.5px;
        }

        .widget-icon {
          font-size: 16px;
          opacity: 0.9;
        }

        .widget-actions {
          display: none;
          gap: 6px;
        }

        .widget-container.edit-mode .widget-actions {
          display: flex;
        }

        .widget-action {
          width: 20px;
          height: 20px;
          border-radius: 50%;
          background: rgba(255, 255, 255, 0.3);
          border: 1px solid rgba(255, 255, 255, 0.4);
          cursor: pointer;
          display: flex;
          align-items: center;
          justify-content: center;
          font-size: 12px;
          transition: all 0.2s ease;
          backdrop-filter: blur(10px);
          -webkit-backdrop-filter: blur(10px);
        }

        .widget-action:hover {
          background: rgba(255, 255, 255, 0.5);
          transform: scale(1.1);
        }

        .widget-action.remove {
          background: rgba(255, 69, 58, 0.8);
          border-color: rgba(255, 69, 58, 1);
          color: white;
        }

        .widget-action.remove:hover {
          background: rgba(255, 69, 58, 1);
        }

        .widget-content {
          flex: 1;
          overflow: hidden;
          padding: 0;
          position: relative;
          min-height: 0;
        }

        .widget-content > * {
          border-radius: inherit;
        }

        /* Dark mode support */
        :host-context([data-theme="dark"]) {
          --widget-bg: rgba(30, 30, 30, 0.6);
          --widget-bg-hover: rgba(40, 40, 40, 0.7);
          --widget-border: rgba(255, 255, 255, 0.15);
          --widget-text-primary: rgba(255, 255, 255, 0.95);
          --widget-text-secondary: rgba(255, 255, 255, 0.70);
          --widget-text-tertiary: rgba(255, 255, 255, 0.50);
          --widget-text-quaternary: rgba(255, 255, 255, 0.30);
        }

        :host-context([data-theme="dark"]) .widget-container {
          background: var(--widget-bg);
          border-color: var(--widget-border);
        }

        :host-context([data-theme="dark"]) .widget-header {
          border-bottom-color: rgba(255, 255, 255, 0.1);
        }

        :host-context([data-theme="dark"]) .widget-title {
          color: var(--widget-text-primary);
        }

        /* Scrollbar styling */
        .widget-content::-webkit-scrollbar {
          width: 6px;
        }

        .widget-content::-webkit-scrollbar-track {
          background: transparent;
        }

        .widget-content::-webkit-scrollbar-thumb {
          background: rgba(255, 255, 255, 0.3);
          border-radius: 3px;
        }

        .widget-content::-webkit-scrollbar-thumb:hover {
          background: rgba(255, 255, 255, 0.5);
        }
      </style>

      <div class="widget-container">
        <div class="widget-header">
          <div class="widget-title">
            <span class="widget-icon">${e.icon}</span>
            <span class="widget-name">${e.name}</span>
          </div>
          <div class="widget-actions">
            <div class="widget-action remove" title="Remove widget">×</div>
          </div>
        </div>
        <div class="widget-content">
          ${this.renderContent()}
        </div>
      </div>
    `,this.shadowRoot.appendChild(t.content.cloneNode(!0)),this.shadowRoot.querySelector(".widget-action.remove")?.addEventListener("click",n=>{n.stopPropagation(),this._instanceId&&s.emit("widget:remove-requested",{instanceId:this._instanceId})})}_getElement(e){return this.shadowRoot.querySelector(e)}_getElements(e){return this.shadowRoot.querySelectorAll(e)}_setupContextMenu(){this._contextMenuHandler=t=>{t.preventDefault(),t.stopPropagation(),s.emit("widget:context-menu",{instanceId:this._instanceId,widgetType:this.getWidgetInfo().type,x:t.clientX,y:t.clientY,element:this})},this.shadowRoot.querySelector(".widget-container")?.addEventListener("contextmenu",this._contextMenuHandler)}_cleanupContextMenu(){const e=this.shadowRoot.querySelector(".widget-container");e&&this._contextMenuHandler&&e.removeEventListener("contextmenu",this._contextMenuHandler)}}class je extends Z{constructor(){super(),this._updateInterval=null,this._mouseDownX=0,this._mouseDownY=0,this._mouseDownTime=0}getWidgetInfo(){return{type:"clock",name:"Clock",description:"Digital clock with date",icon:"🕐"}}renderContent(){const e=new Date,t=this.closest(".widget-instance"),i=t?parseInt(t.style.width):170,n=t?parseInt(t.style.height):90,o=i<=80,a=i<=170&&n<=40,d=i<=170&&n<=90;let l,c,p,u;o?(l={hour:"2-digit",minute:"2-digit"},c=null,p="14px",u="0px"):a?(l={hour:"2-digit",minute:"2-digit",second:"2-digit"},c=null,p="18px",u="0px"):d?(l={hour:"2-digit",minute:"2-digit",second:"2-digit"},c={month:"short",day:"numeric"},p="24px",u="11px"):(l={hour:"2-digit",minute:"2-digit",second:"2-digit"},c={weekday:"long",month:"long",day:"numeric"},p="42px",u="13px");const h=e.toLocaleTimeString("en-US",l),v=c?e.toLocaleDateString("en-US",c):"";return`
      <style>
        .clock {
          display: flex;
          flex-direction: column;
          align-items: center;
          justify-content: center;
          height: 100%;
          gap: 2px;
          padding: 8px;
          box-sizing: border-box;
          cursor: pointer;
          transition: transform 0.1s ease;
        }

        .clock:active {
          transform: scale(0.98);
        }

        .time {
          font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Display', 'Segoe UI', Roboto, sans-serif;
          font-size: ${p};
          font-weight: 600;
          color: rgba(255, 255, 255, 0.95);
          letter-spacing: -1px;
          line-height: 1;
        }

        .date {
          font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Text', 'Segoe UI', Roboto, sans-serif;
          font-size: ${u};
          font-weight: 500;
          color: rgba(255, 255, 255, 0.7);
          text-align: center;
          letter-spacing: 0.2px;
          display: ${c?"block":"none"};
        }

        /* Dark mode */
        :host-context([data-theme="dark"]) .time {
          color: rgba(255, 255, 255, 0.9);
        }

        :host-context([data-theme="dark"]) .date {
          color: rgba(255, 255, 255, 0.6);
        }
      </style>

      <div class="clock">
        <div class="time" id="time">${h}</div>
        <div class="date" id="date">${v}</div>
      </div>
    `}setupEventListeners(){this._updateInterval=setInterval(()=>{this._updateClock()},1e3);const e=this._getElement(".clock");e&&(e.addEventListener("mousedown",t=>{this._mouseDownX=t.clientX,this._mouseDownY=t.clientY,this._mouseDownTime=Date.now()}),e.addEventListener("click",t=>{if(this._editMode)return;const i=Math.abs(t.clientX-this._mouseDownX),n=Math.abs(t.clientY-this._mouseDownY),o=Date.now()-this._mouseDownTime;if(i<5&&n<5&&o<500){t.stopPropagation();const a=document.getElementById("system-settings");a&&a.openPanel("datetime")}}))}cleanup(){this._updateInterval&&clearInterval(this._updateInterval)}_updateClock(){const e=this._getElement("#time"),t=this._getElement("#date");if(!e)return;const i=new Date,n=this.closest(".widget-instance"),o=n?parseInt(n.style.width):170,a=n?parseInt(n.style.height):90,d=o<=80,l=o<=170&&a<=40,c=o<=170&&a<=90;let p,u;d?(p={hour:"2-digit",minute:"2-digit"},u=null):l?(p={hour:"2-digit",minute:"2-digit",second:"2-digit"},u=null):c?(p={hour:"2-digit",minute:"2-digit",second:"2-digit"},u={month:"short",day:"numeric"}):(p={hour:"2-digit",minute:"2-digit",second:"2-digit"},u={weekday:"long",month:"long",day:"numeric"}),e.textContent=i.toLocaleTimeString("en-US",p),t&&u&&(t.textContent=i.toLocaleDateString("en-US",u))}}customElements.define("clock-widget",je);class Ye extends Z{constructor(){super(),this._updateInterval=null}getWidgetInfo(){return{type:"system-info",name:"System Info",description:"System and browser information",icon:"💻"}}renderContent(){const e=C.getRunningApplications(),t=performance.memory?`${Math.round(performance.memory.usedJSHeapSize/1024/1024)} MB`:"N/A",i=performance.memory?`${Math.round(performance.memory.jsHeapSizeLimit/1024/1024)} MB`:"N/A";return`
      <style>
        .system-info {
          display: flex;
          flex-direction: column;
          height: 100%;
          padding: 12px;
          box-sizing: border-box;
          gap: 12px;
        }

        .info-header {
          display: flex;
          align-items: center;
          gap: 8px;
          margin-bottom: 4px;
        }

        .info-icon {
          font-size: 20px;
        }

        .info-title {
          font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Display', 'Segoe UI', Roboto, sans-serif;
          font-size: 14px;
          font-weight: 600;
          color: rgba(255, 255, 255, 0.9);
        }

        .info-grid {
          display: grid;
          grid-template-columns: repeat(2, 1fr);
          gap: 10px;
          flex: 1;
        }

        .info-card {
          background: rgba(255, 255, 255, 0.1);
          border: 1px solid rgba(255, 255, 255, 0.15);
          border-radius: 10px;
          padding: 10px;
          display: flex;
          flex-direction: column;
          gap: 4px;
          transition: all 0.2s ease;
        }

        .info-card:hover {
          background: rgba(255, 255, 255, 0.15);
          border-color: rgba(255, 255, 255, 0.25);
        }

        .info-label {
          font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Text', 'Segoe UI', Roboto, sans-serif;
          font-size: 10px;
          color: rgba(255, 255, 255, 0.6);
          font-weight: 500;
          letter-spacing: 0.3px;
          text-transform: uppercase;
        }

        .info-value {
          font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Display', 'Segoe UI', Roboto, sans-serif;
          font-size: 16px;
          color: rgba(255, 255, 255, 0.95);
          font-weight: 600;
          line-height: 1.2;
        }

        .info-subvalue {
          font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Text', 'Segoe UI', Roboto, sans-serif;
          font-size: 11px;
          color: rgba(255, 255, 255, 0.5);
          font-weight: 400;
        }

        .badge {
          display: inline-flex;
          align-items: center;
          justify-content: center;
          width: fit-content;
          padding: 4px 10px;
          border-radius: 12px;
          background: rgba(0, 122, 255, 0.8);
          color: white;
          font-size: 18px;
          font-weight: 700;
        }

        /* Dark mode */
        :host-context([data-theme="dark"]) .info-card {
          background: rgba(255, 255, 255, 0.05);
          border-color: rgba(255, 255, 255, 0.1);
        }

        :host-context([data-theme="dark"]) .info-card:hover {
          background: rgba(255, 255, 255, 0.1);
          border-color: rgba(255, 255, 255, 0.15);
        }

        :host-context([data-theme="dark"]) .info-label {
          color: rgba(255, 255, 255, 0.5);
        }

        :host-context([data-theme="dark"]) .info-value {
          color: rgba(255, 255, 255, 0.85);
        }
      </style>

      <div class="system-info">
        <div class="info-header">
          <span class="info-icon">💻</span>
          <span class="info-title">System Monitor</span>
        </div>
        <div class="info-grid">
          <div class="info-card">
            <div class="info-label">Platform</div>
            <div class="info-value">${navigator.platform}</div>
          </div>
          <div class="info-card">
            <div class="info-label">Browser</div>
            <div class="info-value">${this._getBrowserName()}</div>
          </div>
          <div class="info-card">
            <div class="info-label">Running Apps</div>
            <div class="info-value badge" id="app-count">${e.length}</div>
          </div>
          <div class="info-card">
            <div class="info-label">Uptime</div>
            <div class="info-value" id="uptime">${this._getUptime()}</div>
          </div>
          <div class="info-card">
            <div class="info-label">Memory Usage</div>
            <div class="info-value" id="memory">${t}</div>
            <div class="info-subvalue">of ${i}</div>
          </div>
          <div class="info-card">
            <div class="info-label">Resolution</div>
            <div class="info-value">${window.screen.width}×${window.screen.height}</div>
          </div>
        </div>
      </div>
    `}setupEventListeners(){this._updateInterval=setInterval(()=>{this._updateInfo()},5e3)}cleanup(){this._updateInterval&&clearInterval(this._updateInterval)}_updateInfo(){const e=this._getElement("#app-count"),t=this._getElement("#memory"),i=this._getElement("#uptime");if(e){const n=C.getRunningApplications();e.textContent=n.length}if(t&&performance.memory){const n=Math.round(performance.memory.usedJSHeapSize/1024/1024);t.textContent=`${n} MB`}i&&(i.textContent=this._getUptime())}_getBrowserName(){const e=navigator.userAgent;return e.includes("Firefox")?"Firefox":e.includes("Chrome")?"Chrome":e.includes("Safari")?"Safari":e.includes("Edge")?"Edge":"Unknown"}_getUptime(){const e=Math.floor(performance.now()/1e3),t=Math.floor(e/3600),i=Math.floor(e%3600/60);return t>0?`${t}h ${i}m`:`${i}m`}}customElements.define("system-info-widget",Ye);function Ge(){b.getApp("system-settings")||b.register({id:"system-settings",name:"System Settings",icon:"⚙️",permissions:{filesystem:!1,network:!1,storage:!0},onLaunch:(r={})=>{const e=r.panel||"desktop";return k.createWindow({appId:"system-settings",title:"System Settings",x:r.x||220,y:r.y||110,width:r.width||920,height:r.height||680,minWidth:760,minHeight:520,content:`<system-settings windowed open active-panel="${e}"></system-settings>`})}})}function Xe(r,e){const t=document.createElement("likes-dock");t.id="main-dock",r.appendChild(t),t.setAttribute("position",e.dock.position),t.setAttribute("size",e.dock.size),e.dock.magnification.enabled&&(t.setAttribute("magnification-enabled","true"),t.setAttribute("magnification-max-size",e.dock.magnification.maxSize)),e.dock.autoHide&&t.setAttribute("auto-hide","true"),t.updateComplete.then(()=>{[{id:"finder",name:"Finder",label:"Finder"},{id:"notes",name:"Notes",label:"Notes"},{id:"aiua",name:"Aiua",label:"Aiua"},{id:"aiua-mesh",name:"Mesh",label:"Mesh"},{id:"aiua-agents",name:"Agents",label:"Agents"},{id:"aiua-components",name:"Parts",label:"Parts"},{id:"aiua-config",name:"Config",label:"Config"},{id:"aiua-catalog",name:"Catalog",label:"Catalog"},{id:"system-settings",name:"Settings",label:"Settings"}].forEach(n=>{t.addIcon({id:n.id,name:n.name,label:n.label,iconUrl:""},!0)})}),t.addEventListener("dock-icon-click",i=>{const n=i.detail.iconId;if(b.isRunning(n))k.showApp(n),b.focus(n);else try{b.launch(n)}catch(o){console.error(`Failed to launch ${n}:`,o)}}),s.on("window:closed",({appId:i})=>{if(!k.hasWindowsForApp(i))try{b.quit(i)}catch{}}),s.on("app:launched",({appId:i})=>t.updateIcon(i,{running:!0})),s.on("app:quit",({appId:i})=>t.updateIcon(i,{running:!1})),s.on("app:focus",({appId:i})=>{const n=k.getWindowsForApp(i);n.length>0&&(n.forEach(o=>{o.getAttribute("state")==="minimized"&&o.setAttribute("state","normal")}),k.focusWindow(n[0].id))})}async function Ve(){try{const e=new AbortController,t=setTimeout(()=>e.abort(),3e3),i=await fetch(window.location.origin,{method:"HEAD",cache:"no-store",signal:e.signal});if(clearTimeout(t),!i.ok)throw new Error("not reachable")}catch{M.show({title:"Cannot Restart",message:"No network connection.",duration:5e3});return}try{const e=await caches.keys();await Promise.all(e.map(t=>caches.delete(t)))}catch{}const r=new URL(window.location.href);r.searchParams.set("_reload",Date.now()),window.location.href=r.toString()}function Je(){m.registerWidget({type:"clock",name:"Clock",description:"Always-on desktop clock",icon:"🕒",componentTag:"clock-widget",appId:"desktop",defaultSize:{width:170,height:170},persistent:!0}),m.registerWidget({type:"system-info",name:"System Info",description:"Desktop system monitor widget",icon:"💻",componentTag:"system-info-widget",appId:"desktop",defaultSize:{width:350,height:190},persistent:!0})}async function Ze(r){if(await Ne(),await Fe(),N.initialize(4),window.desktopManager=N,!r.querySelector("widget-container")){const e=document.createElement("widget-container");e.id="desktop-widget-layer",r.appendChild(e)}Je(),await m.loadLayout(),m.getInstances().length===0&&(m.createInstance({type:"clock",x:48,y:72,width:170,height:170,desktopIndex:0}),m.createInstance({type:"system-info",x:48,y:266,width:350,height:190,desktopIndex:0}))}function Qe(r,e){const t=document.createElement("notification-center");t.id="notification-center",document.body.appendChild(t),s.on("notification:show",i=>M.show(i)),s.on("notification:center:toggle",()=>t.toggle?.())}function Ke(r){r.addEventListener("desktop-click",()=>{r.querySelectorAll("likes-window").forEach(e=>e.removeAttribute("focused")),document.getElementById("main-menubar")?.setActiveApp("desktop","Desktop",null)}),r.addEventListener("window-focus",e=>{e.detail.windowId&&k.focusWindow(e.detail.windowId)}),s.on("window:focused",({windowId:e})=>{const t=document.getElementById(e);t&&r.setActiveWindow(t)}),s.on("window:close",({windowId:e})=>{document.getElementById(e)?.remove()})}function et(r){s.on("menu:about",()=>{w(async()=>{const{SYSTEM_VERSION:e}=await Promise.resolve().then(()=>We);return{SYSTEM_VERSION:e}},void 0).then(({SYSTEM_VERSION:e})=>{k.createWindow({appId:"system",title:"About This Mac",x:200,y:150,width:600,height:400,content:`
          <div style="padding:40px;font-family:var(--font-family-system);text-align:center;color:var(--system-foreground);">
            <h1 style="font-size:64px;margin-bottom:20px;">🍎</h1>
            <h2 style="font-size:24px;font-weight:600;margin-bottom:10px;">macOS ${e.codename} ${e.build}</h2>
            <p style="color:var(--system-foreground-secondary);margin-bottom:20px;">Version ${e.semantic}</p>
            <p style="font-size:14px;color:var(--system-foreground-tertiary);margin-bottom:30px;">Build ${e.hash}</p>
            <div style="border-top:1px solid var(--separator-opaque);padding-top:20px;margin-top:20px;">
              <p style="font-size:14px;color:var(--system-foreground-secondary);">jaredlikes Desktop — Philotic Stack</p>
              <p style="font-size:12px;color:var(--system-foreground-tertiary);margin-top:10px;">Built with Web Components</p>
              <p style="font-size:12px;color:var(--system-foreground-tertiary);margin-top:5px;">${e.timestamp}</p>
            </div>
          </div>
        `})})}),s.on("menu:power",async({action:e})=>{e==="restart"?(M.show({title:"Restarting...",message:"Clearing cache...",duration:1e3}),setTimeout(()=>Ve(),1e3)):M.show({title:"Power Action",message:`${e} not implemented`,duration:3e3})}),s.on("menu:new-window",()=>{const e=document.getElementById("main-menubar")?._currentAppId||"desktop";try{b.launch(e)}catch{}}),s.on("menu:close-window",()=>{const e=document.getElementById("desktop")?.getActiveWindow();e&&k.closeWindow(e.id)}),s.on("menu:minimize",()=>{document.getElementById("desktop")?.getActiveWindow()?.setAttribute("state","minimized")}),s.on("menu:zoom",()=>s.emit("menu:fullscreen")),s.on("menu:bring-all-to-front",()=>{document.querySelectorAll("likes-window").forEach(e=>{e.style.display="block"})}),s.on("menu:fullscreen",()=>{const e=document.getElementById("desktop")?.getActiveWindow();e&&typeof e.toggleFullscreen=="function"&&e.toggleFullscreen()}),s.on("menu:system-settings",()=>{b.launch("system-settings",{panel:"desktop"})}),s.on("menu:app-about",({appId:e,appName:t})=>{k.createWindow({appId:e,title:`About ${t}`,x:200,y:150,width:500,height:350,content:`
        <div style="padding:40px;font-family:var(--font-family-system);text-align:center;color:var(--system-foreground);">
          <h1 style="font-size:48px;margin-bottom:20px;">⚡</h1>
          <h2 style="font-size:24px;font-weight:600;margin-bottom:10px;">${t}</h2>
          <p style="color:var(--system-foreground-secondary);margin-bottom:20px;">Philotic Stack Management</p>
          <p style="font-size:14px;color:var(--system-foreground-tertiary);">Part of jaredlikes Desktop</p>
        </div>
      `})}),s.on("menu:app-hide",({appId:e})=>{k.hideApp(e),M.show({title:"App Hidden",message:"Click the dock icon to show again.",duration:2e3})}),s.on("menu:hide-others",({appId:e})=>k.hideOthers(e)),s.on("menu:show-all",()=>k.showAll()),s.on("menu:app-quit",({appId:e,appName:t})=>{C.terminateApplication(e)?(document.getElementById("desktop")?.querySelectorAll("likes-window").forEach(o=>{o.getAttribute("state")==="fullscreen"&&o.toggleFullscreen?.()}),k.closeAllWindowsForApp(e)):C.getApplicationState(e)?.manifest?.canQuit===!1&&M.show({title:t,message:`You cannot quit ${t}`,duration:2e3})}),s.on("app:launch",({appId:e,options:t})=>{try{b.launch(e,t)}catch(i){console.error(`Failed to launch ${e}:`,i)}}),s.on("app:activated",({appId:e})=>{const t=C.getApplicationState(e),i=t?.manifest?.name||e;document.getElementById("main-menubar")?.setActiveApp(e,i,t?.component||null)})}function tt(r){document.addEventListener("keydown",e=>{if(e.key==="Escape"){document.getElementById("main-menubar")?._closeAllMenus?.();return}if(!(e.metaKey||e.ctrlKey))return;if(e.key==="q"){e.preventDefault();const n=document.getElementById("main-menubar");s.emit("menu:app-quit",{appId:n?._currentAppId||"desktop",appName:n?._appName||"Desktop"})}if(e.key==="w"&&(e.preventDefault(),s.emit("menu:close-window")),e.key==="m"&&(e.preventDefault(),s.emit("menu:minimize")),e.key==="n"&&(e.preventDefault(),s.emit("menu:new-window")),e.key==="h"&&!e.altKey){e.preventDefault();const n=document.getElementById("main-menubar");s.emit("menu:app-hide",{appId:n?._currentAppId||"desktop"})}if(e.key==="h"&&e.altKey){e.preventDefault();const n=document.getElementById("main-menubar");s.emit("menu:hide-others",{appId:n?._currentAppId||"desktop"})}if((e.key==="f"||e.key==="F")&&e.ctrlKey){e.preventDefault(),s.emit("menu:fullscreen");return}document.getElementById("main-menubar")?._currentAppComponent?.handleKeyboardShortcut?.(e)&&(e.preventDefault(),e.stopPropagation())})}async function Y(){F(),console.log("[Philotic] Initializing Likes OS (Philotic edition)..."),Ge(),await $e(),console.log("[Philotic] Aiua management app initialized (⌘⇧A to open)");const r=await Se.load();r.appearance.theme==="dark"?document.documentElement.setAttribute("data-theme","dark"):r.appearance.theme==="light"&&document.documentElement.setAttribute("data-theme","light"),document.documentElement.style.setProperty("--accent-color",`var(--accent-${r.appearance.accentColor})`);const e=document.getElementById("desktop");if(!e){console.error("Desktop element not found!");return}await Ze(e),console.log("[Philotic] Desktop home restored (Finder, Notes, widgets, multiple desktops)"),r.desktop.backgroundImage&&e.setAttribute("background-image",r.desktop.backgroundImage),r.desktop.backgroundColor&&e.setAttribute("background-color",r.desktop.backgroundColor);const t=document.createElement("likes-menubar");t.id="main-menubar",document.body.appendChild(t),t.updateComplete.then(()=>t.setActiveApp("desktop","Desktop",null));const i=document.createElement("system-settings");i.id="system-settings",document.body.appendChild(i),Xe(e,r),Qe(),Ke(e),et(),tt(),console.log("[Philotic] Desktop ready.")}document.readyState==="loading"?document.addEventListener("DOMContentLoaded",Y):Y();window.likesOS={version:()=>{const r=J();return F(),console.table({"System Version":r.system.semantic,"Build Hash":r.system.hash,Codename:`${r.system.codename} ${r.system.build}`}),r}};export{w as _,I as a,b,G as c,C as d,s as e,g as f,it as g,nt as h,m as w};
