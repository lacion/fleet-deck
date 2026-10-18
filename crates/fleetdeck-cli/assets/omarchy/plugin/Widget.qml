import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons

// The bar widget of Fleet Deck: a glyph and `<working> · ⚠<needs_you>`.
// It uses core Quickshell and QtQuick types only. The live smoke of
// omarchy-p4.md section 5 confirms the host types, the properties that the bar
// sets (`bar`, `moduleName`, `settings`) and the click handling.
Item {
  id: root

  property var bar: null
  property string moduleName: "fleetdeck.fleet"
  property var settings: ({})

  readonly property color foreground: Commons.Color.foreground
  readonly property color urgent: Commons.Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: Style.font.family
  readonly property bool hasWork: service.counts.needs_you > 0
  readonly property bool quiet: service.deck === "down" || service.deck === "unknown"
  readonly property color tint: hasWork ? urgent : (quiet ? dim : foreground)

  implicitWidth: row.implicitWidth + Style.space(8)
  implicitHeight: row.implicitHeight

  Commons.Translations { id: strings; directory: Qt.resolvedUrl("i18n") }

  Service { id: service }

  Row {
    id: row
    anchors.centerIn: parent
    spacing: Style.space(6)

    Text {
      textFormat: Text.PlainText
      text: "\uf0e8"
      color: root.tint
      font.family: root.fontFamily
      font.pixelSize: Style.font.icon
    }

    Text {
      textFormat: Text.PlainText
      text: root.hasWork
        ? strings.t("fleetdeck.widget.summary").arg(String(service.counts.working)).arg(String(service.counts.needs_you))
        : strings.t("fleetdeck.widget.quiet").arg(String(service.counts.working))
      color: root.tint
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
    }
  }

  MouseArea {
    anchors.fill: parent
    acceptedButtons: Qt.LeftButton | Qt.RightButton | Qt.MiddleButton
    cursorShape: Qt.PointingHandCursor
    onClicked: function(mouse) {
      if (mouse.button === Qt.RightButton) Quickshell.execDetached(["fleetdeck", "desktop", "open"])
      else if (mouse.button === Qt.MiddleButton) Quickshell.execDetached(["fleetdeck", "desktop", "wall", "show"])
      else Quickshell.execDetached(["omarchy-shell", "shell", "toggle", "fleetdeck.fleet", "{}"])
    }
  }
}
