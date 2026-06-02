import{a as i}from"./index-D9xx1Yf7.js";const s=`
  :host { display: block; height: 100%; overflow-y: auto; }
  .panel { padding: 16px; max-width: 560px; }
  .header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 20px; }
  .title { font-size: 13px; font-weight: 600; color: rgba(255,255,255,0.5); text-transform: uppercase; letter-spacing: 0.05em; }
  .reload-btn { background: rgba(255,255,255,0.07); border: none; border-radius: 5px; color: rgba(255,255,255,0.5); font-size: 11px; padding: 3px 9px; cursor: pointer; font-family: inherit; }
  .reload-btn:hover { background: rgba(255,255,255,0.12); color: #fff; }

  .card { background: rgba(255,255,255,0.04); border: 1px solid rgba(255,255,255,0.07); border-radius: 10px; padding: 20px; }
  .card-desc { font-size: 12px; color: rgba(255,255,255,0.35); line-height: 1.5; margin-bottom: 20px; }

  .field { margin-bottom: 16px; }
  .field-label { font-size: 11px; font-weight: 600; color: rgba(255,255,255,0.4); text-transform: uppercase; letter-spacing: 0.06em; margin-bottom: 6px; }
  .field-input {
    background: rgba(0,0,0,0.3);
    border: 1px solid rgba(255,255,255,0.12);
    border-radius: 6px;
    color: #fff;
    font-size: 13px;
    font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Text', sans-serif;
    padding: 8px 11px;
    outline: none;
    width: 100%;
    box-sizing: border-box;
  }
  .field-input:focus { border-color: rgba(10,132,255,0.6); }
  .field-input::placeholder { color: rgba(255,255,255,0.2); }
  .field-hint { font-size: 11px; color: rgba(255,255,255,0.25); margin-top: 4px; }

  .actions { display: flex; align-items: center; gap: 10px; margin-top: 20px; }
  .btn { border: none; border-radius: 6px; font-size: 13px; font-family: inherit; padding: 7px 16px; cursor: pointer; font-weight: 500; }
  .btn-primary { background: rgba(10,132,255,0.9); color: #fff; }
  .btn-primary:hover { background: #0a84ff; }
  .btn-primary:disabled { opacity: 0.45; cursor: default; }
  .save-status { font-size: 12px; color: rgba(255,255,255,0.4); }
  .save-status.ok { color: #34c759; }
  .save-status.err { color: #ff453a; }

  .loading { font-size: 13px; color: rgba(255,255,255,0.3); padding: 20px 0; }
`;class o extends HTMLElement{constructor(){super(),this._profile=null,this._loading=!0,this._saving=!1,this._status=null}connectedCallback(){this._load()}async _load(){this._loading=!0,this._render();try{this._profile=await i.getUserProfile()}catch{this._profile={timezone:null,display_name:null}}this._loading=!1,this._render()}async _save(){const e=this.shadowRoot?.querySelector("#tz-input")?.value?.trim()||null,t=this.shadowRoot?.querySelector("#dn-input")?.value?.trim()||null;this._saving=!0,this._status=null,this._render();try{this._profile=await i.patchUserProfile({timezone:e||null,display_name:t||null}),this._status={ok:!0,msg:"Saved"}}catch(a){this._status={ok:!1,msg:a.message||"Save failed"}}this._saving=!1,this._render()}_render(){this.shadowRoot||this.attachShadow({mode:"open"});const e=this._profile?.timezone??"",t=this._profile?.display_name??"";this.shadowRoot.innerHTML=`
      <style>${s}</style>
      <div class="panel">
        <div class="header">
          <span class="title">User Profile</span>
          <button class="reload-btn" id="reload-btn">Reload</button>
        </div>
        ${this._loading?'<div class="loading">Loading…</div>':`
        <div class="card">
          <div class="card-desc">
            Hotel-scoped operator profile. These values are injected into every agent's
            cognitive header so models interpret time and identity references correctly.
          </div>

          <div class="field">
            <div class="field-label">Display Name</div>
            <input id="dn-input" class="field-input" type="text"
              placeholder="e.g. Jared"
              value="${t?t.replace(/"/g,"&quot;"):""}">
            <div class="field-hint">How the operator is referred to by agents.</div>
          </div>

          <div class="field">
            <div class="field-label">Timezone</div>
            <input id="tz-input" class="field-input" type="text"
              placeholder="e.g. America/New_York"
              value="${e?e.replace(/"/g,"&quot;"):""}">
            <div class="field-hint">IANA timezone name — appended to the UTC timestamp in every agent's cognitive header.</div>
          </div>

          <div class="actions">
            <button class="btn btn-primary" id="save-btn" ${this._saving?"disabled":""}>
              ${this._saving?"Saving…":"Save"}
            </button>
            ${this._status?`
              <span class="save-status ${this._status.ok?"ok":"err"}">${this._status.msg}</span>
            `:""}
          </div>
        </div>
        `}
      </div>
    `,this.shadowRoot.querySelector("#reload-btn")?.addEventListener("click",()=>this._load()),this.shadowRoot.querySelector("#save-btn")?.addEventListener("click",()=>this._save())}}customElements.define("aiua-user-profile-panel",o);
