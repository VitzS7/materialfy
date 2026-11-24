// Учёт слотов ImageGate ( Md3/ImageGate.qml — тот самый синглтон, не копия):
//   QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input tests/qml/tst_imagegate.qml
// (tests/qml/gate/ImageGate.qml — копия настоящего синглтона: симлинк не работает на Windows
//  (core.symlinks=false), весь модуль Md3 сюда нельзя — его синглтоны тянут Materialfy.Core.
//  Копию при изменении оригинала нужно обновлять: они в одном коммите.)
// Контракт: acquire до limit — слот сразу; дальше — очередь; release будит ждущего через
// qmlContinue; cancel убирает ждущего без возврата слота (слот ему не выдавался);
// лишний release не уводит счётчик в минус; мёртвая заявка пропускается.
import QtQuick
import QtTest
import "gate" as Gate

TestCase {
    name: "ImageGate"

    // фальшивый «Image»: очередь зовёт qmlContinue на том, кого поставила в очередь
    Component {
        id: fakeFactory
        QtObject {
            property int continues: 0
            function qmlContinue() { continues += 1 }
        }
    }

    // занять все слоты лимита; вернуть выданные держатели
    function fillToLimit() {
        const holders = []
        for (let i = 0; i < Gate.ImageGate.limit; ++i) {
            const o = createTemporaryObject(fakeFactory, null)
            compare(Gate.ImageGate.acquire(o), true, "слот " + i + " выдан сразу")
            holders.push(o)
        }
        return holders
    }

    function drainQueue() {
        // состояние синглтона между тестами: выданные слоты отпустить, очередь разобрать
        while (Gate.ImageGate.running > 0)
            Gate.ImageGate.release(null)
        Gate.ImageGate._queue = []
        compare(Gate.ImageGate.running, 0)
        compare(Gate.ImageGate._queue.length, 0)
    }

    function test_acquire_up_to_limit_then_queue() {
        drainQueue()
        const holders = fillToLimit()
        const waiter = createTemporaryObject(fakeFactory, null)
        compare(Gate.ImageGate.acquire(waiter), false, "сверх лимита — в очередь")
        compare(Gate.ImageGate._queue.length, 1)
        for (const h of holders)
            Gate.ImageGate.release(h)
        compare(waiter.continues, 1, "ждущий получил слот через qmlContinue")
        compare(Gate.ImageGate.running, 1)
        drainQueue()
    }

    function test_release_wakes_queue_in_order() {
        drainQueue()
        const holders = fillToLimit()
        const w1 = createTemporaryObject(fakeFactory, null)
        const w2 = createTemporaryObject(fakeFactory, null)
        compare(Gate.ImageGate.acquire(w1), false)
        compare(Gate.ImageGate.acquire(w2), false)
        Gate.ImageGate.release(holders[0])
        compare(w1.continues, 1, "первый ждущий разбужен первым")
        compare(w2.continues, 0, "второй ждёт своего слота")
        compare(Gate.ImageGate.running, Gate.ImageGate.limit)
        Gate.ImageGate.release(w1)
        compare(w2.continues, 1, "освободившийся слот уходит следующему")
        drainQueue()
    }

    function test_cancel_does_not_return_slot() {
        drainQueue()
        const holders = fillToLimit()
        const waiter = createTemporaryObject(fakeFactory, null)
        compare(Gate.ImageGate.acquire(waiter), false)
        Gate.ImageGate.cancel(waiter)         // ушёл, не получив слота
        compare(Gate.ImageGate._queue.length, 0)
        Gate.ImageGate.release(holders[0])    // единственный слот вернулся
        compare(waiter.continues, 0, "отменённый не будится")
        compare(Gate.ImageGate.running, Gate.ImageGate.limit - 1, "слот не достался отменённому")
        drainQueue()
    }

    function test_release_below_zero_is_clamped() {
        drainQueue()
        const o = createTemporaryObject(fakeFactory, null)
        Gate.ImageGate.release(o)             // release без acquire
        compare(Gate.ImageGate.running, 0, "счётчик не ушёл в минус")
        const w = createTemporaryObject(fakeFactory, null)
        compare(Gate.ImageGate.acquire(w), true, "после лишнего release лимит не съеден")
        drainQueue()
    }

    function test_queued_item_without_continue_is_skipped() {
        drainQueue()
        const holders = fillToLimit()
        Gate.ImageGate._queue.push(null)      // «мёртвая» заявка в очереди
        const w = createTemporaryObject(fakeFactory, null)
        compare(Gate.ImageGate.acquire(w), false)
        Gate.ImageGate.release(holders[0])
        compare(w.continues, 1, "мёртвая заявка пропущена, живая разбужена")
        drainQueue()
    }
}
