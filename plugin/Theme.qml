// qmllint disable missing-property
// ^ qs.Commons exposes nested QtObjects (Style.spacing, Style.font) the linter cannot type
import QtQuick
import qs.Commons

QtObject {
    readonly property real ctl: Math.round(Style.spacing.controlHeight)
    readonly property real gap: Math.round(Style.spacing.controlGap)
    readonly property real padX: Style.space(16)
    readonly property real padY: Style.space(12)
    readonly property real radius: Style.cornerRadius
    readonly property real section: Style.space(14)
    readonly property real dim: 0.75
    readonly property string family: Style.font.family
    readonly property real caption: Style.font.caption
    readonly property real bodySmall: Style.font.bodySmall
    readonly property real body: Style.font.body
    readonly property real subtitle: Style.font.subtitle
    readonly property real title: Style.font.title
}
