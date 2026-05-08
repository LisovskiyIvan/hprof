export default function Home() {
  return (
    <div className="min-h-screen bg-gray-50 flex items-center justify-center relative">
      {/* GitHub corner link */}
      <a
        href="https://github.com/LisovskiyIvan"
        className="absolute top-0 right-0"
        target="_blank"
        aria-label="View source on GitHub"
      >
        <svg
          width="80"
          height="80"
          viewBox="0 0 250 250"
          className="absolute top-0 right-0"
          fill="#151513"
          style={{ color: '#fff' }}
        >
          <path d="M0,0 L115,115 L130,115 L142,142 L250,250 L250,0 Z" />
          <path
            d="M128.3,109.0 C113.8,99.7 119.0,89.6 119.0,89.6 C122.0,82.7 120.5,78.6 120.5,78.6 C119.2,72.0 123.4,76.3 123.4,76.3 C127.3,80.9 125.5,87.3 125.5,87.3 C122.9,97.6 130.6,101.9 134.4,103.2"
            fill="currentColor"
            style={{ transformOrigin: '130px 106px' }}
          />
          <path
            d="M115.0,115.0 C114.9,115.1 118.7,116.5 119.8,115.4 L133.7,101.6 C136.9,99.2 139.9,98.4 142.2,98.6 C133.8,88.0 127.5,74.4 143.8,58.0 C148.5,53.4 154.0,51.2 159.7,51.0 C160.3,49.4 163.2,43.6 171.4,40.1 C171.4,40.1 176.1,42.5 178.8,56.2 C183.1,58.6 187.2,61.8 190.9,65.4 C194.5,69.0 197.7,73.2 200.1,77.6 C213.8,80.2 216.3,84.9 216.3,84.9 C212.7,93.1 206.9,96.0 205.4,96.6 C205.1,102.4 203.0,107.8 198.3,112.5 C181.9,128.9 168.3,122.5 157.7,114.1 C157.9,116.9 156.7,120.9 152.7,124.9 L141.0,136.5 C139.8,137.7 141.6,141.9 141.8,141.8 Z"
            fill="currentColor"
          />
        </svg>
      </a>

      <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8 py-12">
        <div className="text-center">
          <h1 className="text-4xl font-bold text-gray-900 sm:text-6xl">Dayme React Template</h1>
          <p className="mt-4 text-xl text-gray-600">
            Быстрый старт React-приложений с настроенными Tailwind CSS и React Router
          </p>

          <div className="mt-8">
            <code className="px-4 py-2 bg-gray-800 text-white rounded-lg">
              npx dayme create my-app
            </code>
          </div>

          <div className="mt-12 grid grid-cols-1 gap-8 sm:grid-cols-2 lg:grid-cols-4">
            <div className="bg-white p-6 rounded-lg shadow-md">
              <h3 className="text-2xl font-semibold">⚡️ Vite</h3>
              <p className="mt-2 text-gray-600 text-lg">Сверхбыстрый инструмент сборки</p>
            </div>

            <div className="bg-white p-6 rounded-lg shadow-md">
              <h3 className="text-2xl font-semibold">🎨 Tailwind CSS</h3>
              <p className="mt-2 text-gray-600 text-lg">Современные утилитарные стили</p>
            </div>

            <div className="bg-white p-6 rounded-lg shadow-md">
              <h3 className="text-2xl font-semibold">📝 TypeScript</h3>
              <p className="mt-2 text-gray-600 text-lg">Типобезопасность из коробки</p>
            </div>

            <div className="bg-white p-6 rounded-lg shadow-md">
              <h3 className="text-2xl font-semibold">🛠️ React Router</h3>
              <p className="mt-2 text-gray-600 text-lg">Маршрутизация и навигация</p>
            </div>
          </div>

          <div className="mt-12">
            <a
              href="https://github.com/LisovskiyIvan/dayme"
              target="_blank"
              className="inline-flex items-center px-6 py-3 border border-transparent text-base font-medium rounded-md text-white bg-indigo-600 hover:bg-indigo-700"
            >
              Начать работу
            </a>
          </div>
        </div>
      </div>
    </div>
  )
}
