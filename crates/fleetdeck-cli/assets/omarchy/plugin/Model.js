// Fleet Deck plugin: the pure parsing and bounding code of the feed lines.
// No Quickshell import. The limits are the ones of omarchy-p4.md section 3.1.
// The plugin bounds every line again, also when the station already did.

var MAX_LINE_BYTES = 16 * 1024
var MAX_NEEDS_YOU = 10
var MAX_STATIONS = 32
var MAX_STATION_NAME = 64
var MAX_CALLSIGN = 32
var MAX_LABEL = 64
var KINDS = ["command", "file_change", "tool", "question"]
var DECKS = ["up", "down", "unknown"]
var ID_PATTERN = /^[A-Za-z0-9_-]{1,64}$/

// The state of a line that is missing, too large or not valid.
function unknownState() {
  return {
    v: 1,
    deck: "unknown",
    counts: { working: 0, idle: 0, needs_you: 0, blocked: 0, lost: 0 },
    needs_you: [],
    usage: null,
    stations: []
  }
}

// The size of a text in UTF-8 bytes.
function utf8Length(text) {
  var bytes = 0
  for (var i = 0; i < text.length; i++) {
    var code = text.charCodeAt(i)
    if (code < 0x80) bytes += 1
    else if (code < 0x800) bytes += 2
    else if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length) {
      bytes += 4
      i += 1
    } else bytes += 3
  }
  return bytes
}

// Remove the control and bidirectional characters, then cut at the limit.
function cleanText(value, max) {
  if (typeof value !== "string") return ""
  var text = value.replace(/[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u2028\u2029\u202a-\u202e\u2066-\u2069]/g, "")
  return text.length > max ? text.substring(0, max) : text
}

function isCount(value) {
  return typeof value === "number" && isFinite(value) && value >= 0 && Math.floor(value) === value
}

function count(value) {
  return isCount(value) ? value : 0
}

function countOrNull(value) {
  return isCount(value) ? value : null
}

function isId(value) {
  return typeof value === "string" && ID_PATTERN.test(value)
}

function parseItem(raw) {
  if (!raw || typeof raw !== "object") return null
  if (!isId(raw.station_id) || !isId(raw.session) || !isId(raw.request)) return null
  if (KINDS.indexOf(raw.kind) < 0) return null
  var callsign = cleanText(raw.callsign, MAX_CALLSIGN)
  return {
    station: cleanText(raw.station, MAX_STATION_NAME),
    station_id: raw.station_id,
    session: raw.session,
    request: raw.request,
    callsign: callsign === "" ? null : callsign,
    kind: raw.kind,
    age_s: count(raw.age_s)
  }
}

function parseStation(raw) {
  if (!raw || typeof raw !== "object") return null
  return {
    name: cleanText(raw.name, MAX_STATION_NAME),
    up: raw.up === true,
    cpu_load_pct: countOrNull(raw.cpu_load_pct)
  }
}

function parseUsage(raw) {
  if (!raw || typeof raw !== "object") return null
  if (typeof raw.label !== "string" || typeof raw.pct !== "number" || !isFinite(raw.pct)) return null
  return {
    label: cleanText(raw.label, MAX_LABEL),
    pct: Math.max(0, Math.min(100, Math.round(raw.pct))),
    resets_in_s: countOrNull(raw.resets_in_s)
  }
}

// Parse one line of `fleetdeck desktop feed`. A bad line gives the unknown state.
function parse(line) {
  if (typeof line !== "string" || utf8Length(line) > MAX_LINE_BYTES) return unknownState()
  var raw
  try {
    raw = JSON.parse(line)
  } catch (error) {
    return unknownState()
  }
  if (!raw || typeof raw !== "object" || raw instanceof Array || raw.v !== 1) return unknownState()
  var state = unknownState()
  state.deck = DECKS.indexOf(raw.deck) >= 0 ? raw.deck : "unknown"
  var counts = raw.counts && typeof raw.counts === "object" ? raw.counts : {}
  state.counts = {
    working: count(counts.working),
    idle: count(counts.idle),
    needs_you: count(counts.needs_you),
    blocked: count(counts.blocked),
    lost: count(counts.lost)
  }
  var items = raw.needs_you instanceof Array ? raw.needs_you : []
  for (var i = 0; i < items.length && state.needs_you.length < MAX_NEEDS_YOU; i++) {
    var item = parseItem(items[i])
    if (item) state.needs_you.push(item)
  }
  var stations = raw.stations instanceof Array ? raw.stations : []
  for (var j = 0; j < stations.length && state.stations.length < MAX_STATIONS; j++) {
    var station = parseStation(stations[j])
    if (station) state.stations.push(station)
  }
  state.usage = parseUsage(raw.usage)
  return state
}
