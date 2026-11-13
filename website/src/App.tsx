import { Background } from './components/Background'
import { Features } from './components/Features'
import { Hero } from './components/Hero'
import { Nav } from './components/Nav'
import { PortalFlow } from './components/PortalFlow'

export default function App() {
  return (
    <div className="relative min-h-dvh overflow-x-hidden text-off-white lg:h-dvh lg:overflow-y-hidden">
      <Background />
      <div className="relative z-10 mx-auto flex min-h-dvh max-w-[90rem] flex-col px-[clamp(1rem,4vw,3rem)] lg:h-full lg:min-h-0">
        <Nav />
        <main className="flex flex-1 flex-col items-center justify-center gap-[clamp(2rem,5vh,3.5rem)] py-[clamp(1rem,3vh,2rem)] lg:grid lg:grid-cols-12 lg:items-center lg:gap-8 lg:py-0">
          <div className="w-full lg:col-span-5 lg:col-start-1 xl:col-start-2 xl:col-span-4">
            <Hero />
          </div>
          <div className="w-full lg:col-span-7 lg:col-start-6 xl:col-span-6 xl:col-start-6">
            <PortalFlow />
          </div>
        </main>
        <Features />
      </div>
    </div>
  )
}
