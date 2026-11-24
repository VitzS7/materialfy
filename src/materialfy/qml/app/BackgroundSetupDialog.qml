import QtQuick
import QtQuick.Layouts
import QtCore as Persist
import Md3
import Materialfy.Core

// Android: что нужно разрешить, чтобы музыка не обрывалась в фоне (оптимизация батареи, уведомления, автозапуск
// у оболочек вроде HyperOS/ColorOS). Сам открывается один раз, если есть что исправить; open() — вручную из настроек.
Item {
    id: root

    property bool opened: false
    // что осталось исправить (пункты MobileShell.backgroundIssues без уже отмеченных вручную)
    property var issues: []
    onVisibleChanged: visible ? BackStack.push(root) : BackStack.pop(root)
    Component.onDestruction: BackStack.pop(root)

    // автозапуск прочитать нельзя: пользователь отмечает его сам; «Больше не показывать» — только окно при запуске
    Persist.Settings {
        id: store
        location: Persist.StandardPaths.writableLocation(Persist.StandardPaths.AppConfigLocation) + "/background-setup.ini"
        category: "Background"
        property bool dismissed: false
        property bool autostartDone: false
    }

    function refresh() {
        if (!MobileShell.android)
            return
        issues = MobileShell.backgroundIssues().filter(id => !(id === "autostart" && store.autostartDone))
    }
    function open() {
        refresh()
        opened = true
    }
    function close() { opened = false }

    // при первом запуске — через пару секунд, когда интерфейс уже на экране
    Timer {
        interval: 2500
        running: MobileShell.android && !store.dismissed
        onTriggered: {
            root.refresh()
            if (root.issues.length > 0)
                root.opened = true
        }
    }
    // вернулись из системных настроек — обновить отметки
    Connections {
        target: Qt.application
        function onStateChanged() {
            if (Qt.application.state === Qt.ApplicationActive && root.opened)
                root.refresh()
        }
    }

    readonly property var info: ({
        battery: {
            icon: "bedtime",
            title: I18n.t("Allow background activity"),
            text: I18n.t("Turn off battery optimization for Materialfy, or Android stops the music when the screen is off. On Xiaomi pick “No restrictions” in the battery saver.")
        },
        notifications: {
            icon: "speaker_notes",
            title: I18n.t("Turn on notifications"),
            text: I18n.t("The player card on the lock screen and in the shade needs notifications.")
        },
        promoted: {
            icon: "picture_in_picture_alt",
            title: I18n.t("Allow live notifications"),
            text: I18n.t("Shows the track and its progress in the status bar chip and the system island.")
        },
        autostart: {
            icon: "play_arrow",
            title: I18n.t("Allow autostart"),
            text: I18n.t("Your phone also keeps a list of apps allowed to run in the background. Turn Materialfy on there.")
        }
    })

    anchors.fill: parent
    visible: opened || scrim.opacity > 0

    Rectangle {
        id: scrim
        anchors.fill: parent
        color: Qt.alpha(Theme.scrim, 0.32)
        opacity: root.opened ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: Theme.motion.effectsFast } }
        MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.AllButtons
            onClicked: root.close()
            onWheel: wheel => {}
        }
    }

    Rectangle {
        id: card
        anchors.centerIn: parent
        width: Math.min(520, parent.width - 48)
        height: Math.min(flick.contentHeight + 48, parent.height - 48)
        radius: Theme.shape.extraLarge
        color: Theme.surfaceContainerHigh
        scale: Theme.reducedMotion || root.opened ? 1 : 0.96
        opacity: root.opened ? 1 : 0
        Behavior on scale { NumberAnimation { duration: Theme.motion.spatialFast; easing.type: Easing.OutCubic } }
        Behavior on opacity { NumberAnimation { duration: Theme.motion.effectsFast } }
        Elevation { target: card; level: 3 }
        MouseArea { anchors.fill: parent; acceptedButtons: Qt.AllButtons }

        Flickable {
            id: flick
            anchors.fill: parent
            anchors.margins: 24
            contentWidth: width
            contentHeight: content.implicitHeight
            boundsBehavior: Flickable.StopAtBounds
            clip: true

            ColumnLayout {
                id: content
                width: flick.width
                spacing: 16

                Label {
                    Layout.fillWidth: true
                    text: I18n.t("Keep the music playing")
                    type: "headlineSmall"
                    wrapMode: Text.WordWrap
                }
                Label {
                    Layout.fillWidth: true
                    visible: root.issues.length > 0
                    text: I18n.t("A few settings stop Android from closing Materialfy in the background:")
                    type: "bodyMedium"
                    color: Theme.fgSurfaceVariant
                    wrapMode: Text.WordWrap
                    elide: Text.ElideNone
                }
                RowLayout {
                    Layout.fillWidth: true
                    visible: root.issues.length === 0
                    spacing: 12
                    Icon { name: "check_circle"; size: 24; color: Theme.primary }
                    Label {
                        Layout.fillWidth: true
                        text: I18n.t("Everything is set. Materialfy can keep playing in the background.")
                        type: "bodyMedium"
                        wrapMode: Text.WordWrap
                        elide: Text.ElideNone
                    }
                }

                Repeater {
                    model: root.issues
                    delegate: RowLayout {
                        id: row
                        required property string modelData
                        readonly property var entry: root.info[modelData] ?? ({ icon: "settings", title: modelData, text: "" })
                        Layout.fillWidth: true
                        spacing: 14
                        Icon {
                            Layout.alignment: Qt.AlignTop
                            name: row.entry.icon
                            size: 24
                            color: Theme.secondary
                        }
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 6
                            Label {
                                Layout.fillWidth: true
                                text: row.entry.title
                                type: "titleSmall"
                                wrapMode: Text.WordWrap
                                elide: Text.ElideNone
                            }
                            Label {
                                Layout.fillWidth: true
                                text: row.entry.text
                                type: "bodySmall"
                                color: Theme.fgSurfaceVariant
                                wrapMode: Text.WordWrap
                                elide: Text.ElideNone
                            }
                            Flow {
                                Layout.fillWidth: true
                                spacing: 8
                                Button {
                                    text: I18n.t("Open settings")
                                    style: "tonal"
                                    size: "xs"
                                    onClicked: MobileShell.openBackgroundSetting(row.modelData)
                                }
                                Button {
                                    visible: row.modelData === "autostart"
                                    text: I18n.t("Done")
                                    style: "text"
                                    size: "xs"
                                    onClicked: {
                                        store.autostartDone = true
                                        root.refresh()
                                    }
                                }
                            }
                        }
                    }
                }

                Flow {
                    Layout.fillWidth: true
                    layoutDirection: Qt.RightToLeft
                    spacing: 8
                    Button { text: I18n.t("Close"); onClicked: root.close() }
                    Button {
                        visible: !store.dismissed
                        text: I18n.t("Don’t show again")
                        style: "text"
                        onClicked: {
                            store.dismissed = true
                            root.close()
                        }
                    }
                }
            }
        }
    }
}
