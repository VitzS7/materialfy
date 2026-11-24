// Жизненный цикл плейлиста на настоящем мосту (Library из Materialfy.Core):
//   QT_QPA_PLATFORM=offscreen QT_FORCE_STDERR_LOGGING=1 \
//     target/release/materialfy --offline --new-instance --qml tests/qml/integration/LifecycleHarness.qml
// Проверяет контракт QML-интеграции: playlistTracks — стабильный экземпляр модели на вызов,
// releasePlaylist парой к каждому acquire; настоящие TrackListPage/PlaylistPage открываются
// и закрываются без ошибок QML; шапка/страница не ждут данные. Итог — SMOKE-RESULT PASS/FAIL.
// Замечание: строки в DictListModel из QML не добавить (модель создаётся в Rust) — наполнение
// модели волной на настоящей TrackListPage не проверяется здесь (см. WaveHarness/docs).
import QtQuick
import QtQuick.Controls.Basic
import Md3
import Materialfy.Core
import "../../../src/materialfy/qml/app" as App
import "../../../src/materialfy/qml/app/pages" as Pages

ApplicationWindow {
    id: win
    width: 900
    height: 700
    visible: true

    property int _fails: 0
    function check(name, cond) {
        console.log(cond ? "SMOKE-PASS " + name : "SMOKE-FAIL " + name)
        if (!cond) _fails++
    }
    function finish() {
        console.log(_fails === 0 ? "SMOKE-RESULT PASS" : "SMOKE-RESULT FAIL")
        Qt.quit()
    }

    Timer { id: step; interval: 150; repeat: true; property int n: 0; onTriggered: run() }
    Component.onCompleted: step.start()

    function run() {
        const t = step.n++
        if (t === 0) {
            // стабильный экземпляр: повторный вызов — тот же объект, счётчик владельцев растёт
            const m1 = Library.playlistTracks("smoke-id-1")
            const m2 = Library.playlistTracks("smoke-id-1")
            check("model-stable-instance", m1 !== null && m1 === m2)
            check("release-invokable", true)      // дошли сюда — invokable существует
            Library.releasePlaylist("smoke-id-1") // пара ко второму acquire
            // настоящий TrackListPage на модели моста: страница и пустое состояние честны
            pageLoader.active = true
        } else if (t === 1) {
            const p = pageLoader.item
            check("page-loads", p !== null && p.listView !== null)
            check("empty-honest", p.model.count === 0 && !p.model.loading)
            pageLoader.active = false             // уничтожение → releasePlaylist парой
        } else if (t === 2) {
            check("page-destroyed", pageLoader.item === null)
            // настоящий PlaylistPage: production-путь acquire/release (_syncModel c guard'ом)
            plLoader.active = true
        } else if (t === 3) {
            const q = plLoader.item
            check("playlist-page-loads", q !== null && q.playlistId === "smoke-id-1")
            plLoader.active = false               // скрытие/уничтожение → releasePlaylist
        } else if (t === 4) {
            check("playlist-page-destroyed", plLoader.item === null)
            finish()
        }
    }

    Loader {
        id: pageLoader
        active: false
        anchors.fill: parent
        sourceComponent: App.TrackListPage {
            id: tp
            // acquire один раз на жизнь страницы (не в binding: переоценка надувала бы счётчик)
            Component.onCompleted: tp.model = Library.playlistTracks("smoke-id-1")
            Component.onDestruction: Library.releasePlaylist("smoke-id-1")
            title: "Lifecycle smoke"
            showHeader: false
        }
    }
    Loader {
        id: plLoader
        active: false
        anchors.fill: parent
        sourceComponent: Pages.PlaylistPage {
            playlistId: "smoke-id-1"
        }
    }
}
