// Philotic surface renderer (doc:desktop-generative-surfaces, seam
// surface-web-renderer). Draws one A2UI v0.9 surface from the hotel's
// validated record. UI is data: every node is built with createElement and
// textContent (never by parsing HTML strings), and only components in the philotic.desktop.v1
// catalog render — anything else fails the whole surface loudly.
//
// Hosts plug in through window.PhiloticSurfaceHost.sendAction(action); the
// standalone page posts to the hotel. Apple webview and Telegram Mini App
// hosts arrive in S3.
(function () {
  "use strict";

  var root = document.getElementById("surface");
  var footer = document.getElementById("attribution");

  function el(tag, cls, text) {
    var node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text !== undefined && text !== null) node.textContent = String(text);
    return node;
  }

  function showError(message) {
    root.replaceChildren(el("p", "error", message));
  }

  // ── Path parsing: /s/<target>/<surface_id> ─────────────────────────────
  var parts = window.location.pathname.split("/").filter(Boolean);
  if (parts.length !== 3 || parts[0] !== "s") {
    showError("This link does not name a surface.");
    return;
  }
  var target = decodeURIComponent(parts[1]);
  var surfaceId = decodeURIComponent(parts[2]);

  // ── JSON Pointer (RFC 6901) ────────────────────────────────────────────
  function tokens(pointer) {
    if (pointer === "" || pointer === "/") return [];
    return pointer
      .slice(1)
      .split("/")
      .map(function (t) { return t.replace(/~1/g, "/").replace(/~0/g, "~"); });
  }
  function getAt(doc, pointer) {
    var cur = doc;
    var ts = tokens(pointer);
    for (var i = 0; i < ts.length; i++) {
      if (cur === null || typeof cur !== "object") return undefined;
      cur = cur[ts[i]];
    }
    return cur;
  }
  function setAt(doc, pointer, value) {
    var ts = tokens(pointer);
    if (!ts.length) return;
    var cur = doc;
    for (var i = 0; i < ts.length - 1; i++) {
      if (cur[ts[i]] === null || typeof cur[ts[i]] !== "object") cur[ts[i]] = {};
      cur = cur[ts[i]];
    }
    cur[ts[ts.length - 1]] = value;
  }

  // ── Host bridge ────────────────────────────────────────────────────────
  var defaultHost = {
    name: "web",
    sendAction: function (action) {
      return fetch(
        "/api/mesh/targets/" + encodeURIComponent(target) + "/surfaces/" +
          encodeURIComponent(surfaceId) + "/actions",
        {
          method: "POST",
          credentials: "same-origin",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ version: "v0.9", action: action }),
        }
      ).then(function (res) {
        if (res.status === 404 || res.status === 405 || res.status === 501) {
          throw new Error("This view cannot send button presses yet.");
        }
        if (!res.ok) throw new Error("The hotel refused the action (" + res.status + ").");
      });
    },
  };
  function host() {
    return window.PhiloticSurfaceHost || defaultHost;
  }

  // ── Renderer ───────────────────────────────────────────────────────────
  function render(state, catalog) {
    var components = state.components || {};
    var model = state.dataModel || state.data_model || {};
    var allowed = catalog.components || {};
    var depth = 0;

    function resolve(value, item) {
      if (value && typeof value === "object" && !Array.isArray(value) && typeof value.path === "string") {
        return value.path.charAt(0) === "/" ? getAt(model, value.path) : getAt(item, "/" + value.path);
      }
      return value;
    }
    function text(value, item) {
      var v = resolve(value, item);
      return v === undefined || v === null ? "" : typeof v === "object" ? JSON.stringify(v) : String(v);
    }
    function bindTarget(value, item) {
      if (value && typeof value === "object" && typeof value.path === "string") {
        if (value.path.charAt(0) === "/") {
          return function (v) { setAt(model, value.path, v); };
        }
        if (item && typeof item === "object") {
          return function (v) { setAt(item, "/" + value.path, v); };
        }
      }
      return function () {};
    }
    function childList(spec, item) {
      if (Array.isArray(spec)) return spec.map(function (id) { return [id, item]; });
      if (spec && typeof spec === "object") {
        var items = spec.path.charAt(0) === "/" ? getAt(model, spec.path) : getAt(item, "/" + spec.path);
        return (Array.isArray(items) ? items : []).map(function (it) { return [spec.componentId, it]; });
      }
      return [];
    }

    function node(id, item) {
      var c = components[id];
      if (!c) throw new Error("missing component '" + id + "'");
      if (!Object.prototype.hasOwnProperty.call(allowed, c.component)) {
        throw new Error(c.component + " is not allowed on surfaces");
      }
      if (++depth > (catalog.limits ? catalog.limits.maxDepth * 4 : 64)) {
        throw new Error("surface nests too deeply");
      }
      var out;
      switch (c.component) {
        case "Text": {
          var tag = { h1: "h1", h2: "h2", h3: "h3", h4: "h4", h5: "h5", caption: "small" }[c.variant] || "p";
          out = el(tag, "text", text(c.text, item));
          break;
        }
        case "Column":
        case "Row":
        case "List": {
          var dir = c.component === "Row" || c.direction === "horizontal" ? "row" : "column";
          out = el("div", "stack " + dir + " justify-" + (c.justify || "start") + " align-" + (c.align || "stretch"));
          childList(c.children, item).forEach(function (pair) { out.appendChild(node(pair[0], pair[1])); });
          break;
        }
        case "Card":
          out = el("section", "card");
          out.appendChild(node(c.child, item));
          break;
        case "Divider":
          out = el(c.axis === "vertical" ? "div" : "hr", c.axis === "vertical" ? "vdivider" : "divider");
          break;
        case "Button": {
          out = el("button", "button " + (c.variant || "default"));
          out.type = "button";
          out.appendChild(node(c.child, item));
          out.addEventListener("click", function () {
            var event = (c.action && c.action.event) || {};
            var context = {};
            Object.keys(event.context || {}).forEach(function (k) {
              context[k] = resolve(event.context[k], item);
            });
            out.disabled = true;
            Promise.resolve()
              .then(function () {
                return host().sendAction({
                  name: event.name,
                  surfaceId: surfaceId,
                  sourceComponentId: c.id,
                  timestamp: new Date().toISOString(),
                  context: context,
                });
              })
              .then(function () { out.classList.add("sent"); })
              .catch(function (err) { flash(err.message); })
              .then(function () { out.disabled = false; });
          });
          break;
        }
        case "TextField": {
          out = el("label", "field");
          out.appendChild(el("span", "label", text(c.label, item)));
          var input = el(c.variant === "longText" ? "textarea" : "input", "input");
          if (c.variant === "obscured") input.type = "password";
          if (c.variant === "number") input.type = "number";
          if (c.validationRegexp && input.tagName === "INPUT") input.pattern = c.validationRegexp;
          input.value = text(c.value, item);
          var setText = bindTarget(c.value, item);
          input.addEventListener("input", function () {
            setText(c.variant === "number" ? Number(input.value) : input.value);
          });
          out.appendChild(input);
          break;
        }
        case "CheckBox": {
          out = el("label", "check");
          var box = el("input");
          box.type = "checkbox";
          box.checked = Boolean(resolve(c.value, item));
          var setBool = bindTarget(c.value, item);
          box.addEventListener("change", function () { setBool(box.checked); });
          out.appendChild(box);
          out.appendChild(el("span", "", text(c.label, item)));
          break;
        }
        case "ChoicePicker": {
          out = el("fieldset", "choices");
          if (c.label) out.appendChild(el("legend", "", text(c.label, item)));
          var multi = c.variant === "multipleSelection";
          var current = resolve(c.value, item);
          var selected = Array.isArray(current) ? current.slice() : [];
          var setChoice = bindTarget(c.value, item);
          var group = "choice-" + c.id;
          (c.options || []).forEach(function (opt) {
            var row = el("label", "check");
            var input = el("input");
            input.type = multi ? "checkbox" : "radio";
            input.name = group;
            input.checked = selected.indexOf(opt.value) >= 0;
            input.addEventListener("change", function () {
              if (multi) {
                selected = selected.filter(function (v) { return v !== opt.value; });
                if (input.checked) selected.push(opt.value);
              } else {
                selected = [opt.value];
              }
              setChoice(selected.slice());
            });
            row.appendChild(input);
            row.appendChild(el("span", "", text(opt.label, item)));
            out.appendChild(row);
          });
          break;
        }
        case "Slider": {
          out = el("label", "field");
          if (c.label) out.appendChild(el("span", "label", text(c.label, item)));
          var range = el("input", "range");
          range.type = "range";
          range.min = String(c.min || 0);
          range.max = String(c.max);
          range.value = text(c.value, item);
          var readout = el("output", "", range.value);
          var setNum = bindTarget(c.value, item);
          range.addEventListener("input", function () {
            readout.textContent = range.value;
            setNum(Number(range.value));
          });
          out.appendChild(range);
          out.appendChild(readout);
          break;
        }
        case "Modal": {
          out = el("div", "modal");
          var dialog = el("dialog", "dialog");
          dialog.appendChild(node(c.content, item));
          var close = el("button", "button borderless", "Close");
          close.type = "button";
          close.addEventListener("click", function () { dialog.close(); });
          dialog.appendChild(close);
          var trigger = node(c.trigger, item);
          trigger.addEventListener("click", function () { dialog.showModal(); });
          out.appendChild(trigger);
          out.appendChild(dialog);
          break;
        }
        case "Table": {
          out = el("div", "table-wrap");
          var table = el("table", "table");
          var head = el("tr");
          (c.columns || []).forEach(function (col) { head.appendChild(el("th", "", text(col.header, item))); });
          var thead = el("thead");
          thead.appendChild(head);
          table.appendChild(thead);
          var body = el("tbody");
          var rows = resolve(c.rows, item);
          (Array.isArray(rows) ? rows : []).forEach(function (r) {
            var tr = el("tr");
            (c.columns || []).forEach(function (col) {
              var v = r && typeof r === "object" ? r[col.field] : undefined;
              tr.appendChild(el("td", "", v === undefined || v === null ? "" : typeof v === "object" ? JSON.stringify(v) : v));
            });
            body.appendChild(tr);
          });
          table.appendChild(body);
          out.appendChild(table);
          break;
        }
        default:
          throw new Error(c.component + " has no renderer");
      }
      if (typeof c.weight === "number") out.style.flexGrow = String(c.weight);
      if (c.accessibility && c.accessibility.label) {
        out.setAttribute("aria-label", text(c.accessibility.label, item));
      }
      depth--;
      return out;
    }

    return node("root", undefined);
  }

  var flashTimer = null;
  function flash(message) {
    var note = document.getElementById("flash") || el("p", "flash");
    note.id = "flash";
    note.textContent = message;
    document.body.appendChild(note);
    clearTimeout(flashTimer);
    flashTimer = setTimeout(function () { note.remove(); }, 4000);
  }

  function attribution(surface) {
    var when = surface.updated_at ? new Date(surface.updated_at * 1000).toLocaleString() : "";
    footer.textContent =
      (surface.title ? surface.title + " · " : "") +
      "by " + surface.owner_agent_id + " on " + surface.source_hotel +
      (when ? " · updated " + when : "");
  }

  function load() {
    var catalogReq = fetch("/surface-ui/catalog.json", { credentials: "same-origin" }).then(function (r) {
      if (!r.ok) throw new Error("The surface catalog is unavailable.");
      return r.json();
    });
    var surfaceReq = fetch(
      "/api/mesh/targets/" + encodeURIComponent(target) + "/surfaces/" + encodeURIComponent(surfaceId),
      { credentials: "same-origin" }
    ).then(function (r) {
      if (r.status === 401 || r.status === 403) throw new Error("Sign in to the operator console to see this surface.");
      if (r.status === 404) throw new Error("This surface no longer exists.");
      if (!r.ok) throw new Error("The hotel could not load this surface (" + r.status + ").");
      return r.json();
    });
    Promise.all([catalogReq, surfaceReq])
      .then(function (results) {
        var catalog = results[0];
        var surface = results[1].surface;
        if (surface.status === "deleted") throw new Error("This surface was deleted.");
        if (surface.state.theme && surface.state.theme.primaryColor) {
          document.documentElement.style.setProperty("--accent", surface.state.theme.primaryColor);
        }
        document.title = surface.title || "Surface";
        root.replaceChildren(render(surface.state, catalog));
        attribution(surface);
      })
      .catch(function (err) { showError(err.message); });
  }

  load();
})();
