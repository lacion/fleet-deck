import QtQuick
import QtQuick.Shapes
import Quickshell
import qs.Commons
import qs.Commons as Commons

// The bar widget of Fleet Deck: the mark and `<working> · ⚠<needs_you>`.
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

    // The Fleet Deck mark (web/src/ui/brand.ts MARK_PATH), in the tint of the widget.
    Shape {
      id: mark
      width: Style.font.icon
      height: Style.font.icon
      anchors.verticalCenter: parent.verticalCenter
      preferredRendererType: Shape.CurveRenderer
      ShapePath {
        fillColor: root.tint
        fillRule: ShapePath.OddEvenFill
        strokeWidth: -1
        scale: Qt.size(mark.width / 24, mark.height / 24)
        PathSvg { path: "M10.5 2H13.5A2 2 0 0 1 15.5 4V7A2 2 0 0 1 13.5 9H10.5A2 2 0 0 1 8.5 7V4A2 2 0 0 1 10.5 2ZM5.6 10H7.9A1.6 1.6 0 0 1 9.5 11.6V13.9A1.6 1.6 0 0 1 7.9 15.5H5.6A1.6 1.6 0 0 1 4 13.9V11.6A1.6 1.6 0 0 1 5.6 10ZM16.1 10H18.4A1.6 1.6 0 0 1 20 11.6V13.9A1.6 1.6 0 0 1 18.4 15.5H16.1A1.6 1.6 0 0 1 14.5 13.9V11.6A1.6 1.6 0 0 1 16.1 10ZM2 17H4A1.5 1.5 0 0 1 5.5 18.5V20.5A1.5 1.5 0 0 1 4 22H2A1.5 1.5 0 0 1 0.5 20.5V18.5A1.5 1.5 0 0 1 2 17ZM20 17H22A1.5 1.5 0 0 1 23.5 18.5V20.5A1.5 1.5 0 0 1 22 22H20A1.5 1.5 0 0 1 18.5 20.5V18.5A1.5 1.5 0 0 1 20 17Z" }
      }
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
