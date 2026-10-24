import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import qs.Commons
import qs.Commons as Commons
import "Model.js" as Model

// The panel of Fleet Deck: the sessions that need you, the fleet counts, the
// stations and the usage line. It has no control that answers a request.
// It uses core Quickshell and QtQuick types only (a PanelWindow of the layer
// shell). The live smoke of omarchy-p4.md section 5 confirms the host types,
// the keyboard focus and `keepLoaded`.
Item {
  id: root

  property bool opened: false
  property var summary: Model.unknownState()
  property int cursor: 0

  readonly property var items: summary.needs_you
  readonly property color foreground: Commons.Color.foreground
  readonly property color urgent: Commons.Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: Style.font.family

  // The host calls open(payloadJson) and close().
  function open(payloadJson) {
    summary = Model.unknownState()
    cursor = 0
    opened = true
    feed.running = true
    Qt.callLater(function() { keys.forceActiveFocus() })
  }

  function close() {
    opened = false
    restartTimer.stop()
    feed.running = false
  }

  function handleLine(line) {
    summary = Model.parse(line)
    if (cursor >= items.length) cursor = Math.max(0, items.length - 1)
  }

  function move(delta) {
    if (items.length === 0) return
    cursor = Math.max(0, Math.min(items.length - 1, cursor + delta))
  }

  // The ids come from the bounded model only. Each value is one argv entry.
  function openSelected() {
    if (cursor < 0 || cursor >= items.length) return
    var item = items[cursor]
    Quickshell.execDetached(["fleetdeck", "desktop", "open", item.station_id + "/" + item.session, "--request", item.request])
  }

  function handleKey(event) {
    if (event.key === Qt.Key_Escape) root.close()
    else if (event.key === Qt.Key_Down || event.text === "j") root.move(1)
    else if (event.key === Qt.Key_Up || event.text === "k") root.move(-1)
    else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) root.openSelected()
    else if (event.text === "w") Quickshell.execDetached(["fleetdeck", "desktop", "wall", "next"])
    else return
    event.accepted = true
  }

  function ageText(seconds) {
    if (seconds < 60) return strings.t("fleetdeck.age.seconds").arg(String(seconds))
    if (seconds < 3600) return strings.t("fleetdeck.age.minutes").arg(String(Math.floor(seconds / 60)))
    if (seconds < 86400) return strings.t("fleetdeck.age.hours").arg(String(Math.floor(seconds / 3600)))
    return strings.t("fleetdeck.age.days").arg(String(Math.floor(seconds / 86400)))
  }

  function kindText(kind) {
    if (kind === "command") return strings.t("fleetdeck.kind.command")
    if (kind === "file_change") return strings.t("fleetdeck.kind.file_change")
    if (kind === "tool") return strings.t("fleetdeck.kind.tool")
    return strings.t("fleetdeck.kind.question")
  }

  function rowText(item) {
    var who = item.callsign !== null ? item.callsign : item.session.substring(0, 8)
    return strings.t("fleetdeck.panel.row").arg(who).arg(kindText(item.kind)).arg(item.station).arg(ageText(item.age_s))
  }

  function countsText() {
    var c = summary.counts
    return strings.t("fleetdeck.panel.counts").arg(String(c.working)).arg(String(c.idle)).arg(String(c.needs_you)).arg(String(c.blocked)).arg(String(c.lost))
  }

  function stationText(station) {
    if (!station.up) return strings.t("fleetdeck.panel.station_down").arg(station.name)
    if (station.cpu_load_pct === null) return strings.t("fleetdeck.panel.station_up_no_load").arg(station.name)
    return strings.t("fleetdeck.panel.station_up").arg(station.name).arg(String(station.cpu_load_pct))
  }

  function usageText(usage) {
    if (usage.resets_in_s === null) return strings.t("fleetdeck.panel.usage").arg(usage.label).arg(String(usage.pct))
    return strings.t("fleetdeck.panel.usage_resets").arg(usage.label).arg(String(usage.pct)).arg(ageText(usage.resets_in_s))
  }

  Commons.Translations { id: strings; directory: Qt.resolvedUrl("i18n") }

  Timer {
    id: restartTimer
    interval: 5000
    repeat: false
    onTriggered: if (root.opened) feed.running = true
  }

  // The panel runs its own feed, without --notify, only while it is open.
  Process {
    id: feed
    running: false
    command: ["fleetdeck", "desktop", "feed"]
    stdout: SplitParser {
      onRead: function(data) { root.handleLine(data) }
    }
    onExited: function(exitCode) {
      if (root.opened && exitCode !== 0) {
        root.summary = Model.unknownState()
        restartTimer.start()
      }
    }
  }

  PanelWindow {
    id: window
    visible: root.opened
    color: Commons.Color.background
    implicitWidth: Style.space(420)
    implicitHeight: Style.space(520)
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.namespace: "fleetdeck-panel"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.OnDemand

    FocusScope {
      id: keys
      anchors.fill: parent
      focus: true
      Keys.onPressed: function(event) { root.handleKey(event) }

      Flickable {
        anchors.fill: parent
        anchors.margins: Style.space(12)
        contentWidth: width
        contentHeight: column.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds

        Column {
          id: column
          width: parent.width
          spacing: Style.space(8)

          Text {
            textFormat: Text.PlainText
            width: parent.width
            text: strings.t("fleetdeck.panel.title")
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
          }

          Text {
            textFormat: Text.PlainText
            visible: root.summary.deck !== "up"
            width: parent.width
            text: root.summary.deck === "down" ? strings.t("fleetdeck.panel.deck_down") : strings.t("fleetdeck.panel.deck_unknown")
            color: root.urgent
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            wrapMode: Text.WordWrap
          }

          Text {
            textFormat: Text.PlainText
            width: parent.width
            text: strings.t("fleetdeck.panel.needs_you_title")
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }

          Text {
            textFormat: Text.PlainText
            visible: root.items.length === 0
            width: parent.width
            text: strings.t("fleetdeck.panel.empty")
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }

          Repeater {
            model: root.items

            Item {
              width: column.width
              height: rowLabel.implicitHeight + Style.space(8)

              Rectangle {
                anchors.fill: parent
                visible: index === root.cursor
                color: Commons.Color.accent
                opacity: 0.25
              }

              Text {
                id: rowLabel
                textFormat: Text.PlainText
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                anchors.margins: Style.space(4)
                text: root.rowText(modelData)
                color: root.urgent
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                elide: Text.ElideRight
              }
            }
          }

          Text {
            textFormat: Text.PlainText
            width: parent.width
            text: root.countsText()
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            wrapMode: Text.WordWrap
          }

          Text {
            textFormat: Text.PlainText
            width: parent.width
            text: strings.t("fleetdeck.panel.stations_title")
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }

          Repeater {
            model: root.summary.stations

            Text {
              textFormat: Text.PlainText
              width: column.width
              text: root.stationText(modelData)
              color: modelData.up ? root.foreground : root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              elide: Text.ElideRight
            }
          }

          Text {
            textFormat: Text.PlainText
            visible: root.summary.usage !== null
            width: parent.width
            text: root.summary.usage !== null ? root.usageText(root.summary.usage) : ""
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            wrapMode: Text.WordWrap
          }

          Text {
            textFormat: Text.PlainText
            width: parent.width
            text: strings.t("fleetdeck.panel.hints")
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            wrapMode: Text.WordWrap
          }
        }
      }
    }
  }
}
